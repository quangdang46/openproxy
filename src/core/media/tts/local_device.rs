//! Local-device TTS — shells out to platform native synthesizers.
//!
//! Mac: `say -v <voice> -o tmp.aiff <text>` followed by `ffmpeg` to mp3.
//! Windows: PowerShell + `System.Speech.Synthesis.SpeechSynthesizer`.
//! Linux: not supported (returns an error).
//!
//! NB: the upstream JS implementation depends on `ffmpeg` being on PATH
//! on macOS. This Rust port keeps the same dependency for now.

use async_trait::async_trait;
use base64::Engine as _;
use reqwest::Client;
use std::path::PathBuf;
use std::time::SystemTime;
use tokio::process::Command;

use super::base::{TtsAdapter, TtsError, TtsRequest, TtsResult};

pub struct LocalDeviceAdapter;
pub static ADAPTER: LocalDeviceAdapter = LocalDeviceAdapter;

fn temp_dir() -> PathBuf {
    let mut p = std::env::temp_dir();
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    p.push(format!("openproxy-tts-{nonce}"));
    p
}

async fn synthesize_mac(text: &str, voice_id: &str) -> Result<Vec<u8>, TtsError> {
    let dir = temp_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| TtsError::Network(format!("mkdir tmp: {e}")))?;
    let aiff = dir.join("out.aiff");
    let mp3 = dir.join("out.mp3");

    let mut say = Command::new("say");
    if !voice_id.is_empty() {
        say.arg("-v").arg(voice_id);
    }
    let status = say
        .arg("-o")
        .arg(&aiff)
        .arg(text)
        .status()
        .await
        .map_err(|e| TtsError::Network(format!("say: {e}")))?;
    if !status.success() {
        let _ = tokio::fs::remove_dir_all(&dir).await;
        return Err(TtsError::Network(format!("say exited with {status}")));
    }

    let status = Command::new("ffmpeg")
        .args(["-y", "-i"])
        .arg(&aiff)
        .args(["-codec:a", "libmp3lame", "-qscale:a", "4"])
        .arg(&mp3)
        .status()
        .await
        .map_err(|e| TtsError::Network(format!("ffmpeg: {e}")))?;
    let bytes_result = if status.success() {
        tokio::fs::read(&mp3)
            .await
            .map_err(|e| TtsError::Network(format!("read mp3: {e}")))
    } else {
        Err(TtsError::Network(format!("ffmpeg exited with {status}")))
    };
    let _ = tokio::fs::remove_dir_all(&dir).await;
    bytes_result
}

async fn synthesize_windows(text: &str, voice_id: &str) -> Result<Vec<u8>, TtsError> {
    let dir = temp_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| TtsError::Network(format!("mkdir tmp: {e}")))?;
    let wav = dir.join("out.wav");
    let voice_setup = if voice_id.is_empty() {
        String::new()
    } else {
        format!("$s.SelectVoice('{}');", voice_id.replace('\'', ""))
    };
    let safe_text = text.replace('\'', "''");
    let script = format!(
        "Add-Type -AssemblyName System.Speech; \
         $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; \
         {voice_setup} \
         $s.SetOutputToWaveFile('{path}'); \
         $s.Speak('{safe_text}'); \
         $s.Dispose();",
        path = wav.to_string_lossy(),
    );
    let status = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
        ])
        .arg(&script)
        .status()
        .await
        .map_err(|e| TtsError::Network(format!("powershell: {e}")))?;
    let bytes_result = if status.success() {
        tokio::fs::read(&wav)
            .await
            .map_err(|e| TtsError::Network(format!("read wav: {e}")))
    } else {
        Err(TtsError::Network(format!(
            "powershell exited with {status}"
        )))
    };
    let _ = tokio::fs::remove_dir_all(&dir).await;
    bytes_result
}

#[async_trait]
impl TtsAdapter for LocalDeviceAdapter {
    fn no_auth(&self) -> bool {
        true
    }

    async fn synthesize(
        &self,
        _client: &Client,
        request: &TtsRequest<'_>,
    ) -> Result<TtsResult, TtsError> {
        let bytes = match std::env::consts::OS {
            "macos" => synthesize_mac(request.text, request.model).await?,
            "windows" => synthesize_windows(request.text, request.model).await?,
            other => {
                return Err(TtsError::Network(format!(
                    "local-device TTS: unsupported OS {other}"
                )))
            }
        };
        let format = if std::env::consts::OS == "windows" {
            "wav"
        } else {
            "mp3"
        };
        Ok(TtsResult {
            base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            format: format.to_string(),
        })
    }
}
