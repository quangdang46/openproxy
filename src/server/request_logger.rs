use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;

use chrono::Local;
use serde_json::json;

use crate::server::console_logs::shared_console_log_buffer;

/// Whether ENABLE_REQUEST_LOGS=true was set at process start.
static REQUEST_LOG_FILE_ENABLED: OnceLock<bool> = OnceLock::new();

/// Returns the directory path for request log files, defaulting to `logs/`.
fn log_dir() -> PathBuf {
    std::env::var("REQUEST_LOG_DIR")
        .ok()
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("logs"))
}

/// Append a single JSON line to the daily-rotated request log file.
///
/// The file is named `request-YYYY-MM-DD.log` inside `log_dir()` and is
/// created atomically on first write.  Every call opens, writes, and
/// flushes — no open file handle is retained between calls so the file
/// can be safely rotated (renamed/deleted) by an external process.
pub fn append_request_log(
    method: &str,
    path: &str,
    status: u16,
    duration_ms: u64,
    model: Option<&str>,
) {
    if !enabled() {
        return;
    }

    let dir = log_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("[request_logger] failed to create log dir {:?}: {}", dir, e);
        return;
    }

    let date = Local::now().format("%Y-%m-%d");
    let filename = dir.join(format!("request-{}.log", date));

    let entry = json!({
        "timestamp": Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string(),
        "method": method,
        "path": path,
        "status": status,
        "duration_ms": duration_ms,
        "model": model,
    });

    // Open in append/create mode and write one JSON line.
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&filename)
    {
        Ok(mut file) => {
            let line = serde_json::to_string(&entry).unwrap_or_default();
            let _ = writeln!(file, "{}", line);
            let _ = file.flush();
        }
        Err(e) => {
            eprintln!(
                "[request_logger] failed to open log file {:?}: {}",
                filename, e
            );
        }
    }
}

fn enabled() -> bool {
    *REQUEST_LOG_FILE_ENABLED
        .get_or_init(|| std::env::var("ENABLE_REQUEST_LOGS").ok().as_deref() == Some("true"))
}

/// Structured request/response logging matching 9router's logger.js.
///
/// Logs are printed to stderr with emoji icons and timing:
///   `[HH:MM:SS] 📥 POST /v1/messages model=...`
///   `[HH:MM:SS] 📤 200 (1234ms) POST /v1/messages`
///   `[HH:MM:SS] 💥 404 (42ms) POST /v1/messages`
///   `[HH:MM:SS] 🌊 [STREAM] event.type ...`
///
/// Logs are also broadcast to the SSE console-log stream so the dashboard
/// at `/dashboard/console-log` shows them in real time.

fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            while let Some(n) = chars.next() {
                if n == 'm' {
                    break;
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}

fn log_both(terminal_line: &str) {
    eprintln!("{}", terminal_line);
    let clean = strip_ansi(terminal_line);
    shared_console_log_buffer().append_line_blocking(clean);
}

pub struct RequestLog {
    method: &'static str,
    path: String,
    model: Option<String>,
    start: Instant,
}

impl RequestLog {
    pub fn start(method: &'static str, path: &str, model: Option<&str>) -> Self {
        let time = Local::now().format("%H:%M:%S");
        match model {
            Some(m) => log_both(&format!(
                "\x1b[36m[{}] 📥 {} {} model={}\x1b[0m",
                time, method, path, m
            )),
            None => log_both(&format!("\x1b[36m[{}] 📥 {} {}\x1b[0m", time, method, path)),
        }
        Self {
            method,
            path: path.to_owned(),
            model: model.map(str::to_string),
            start: Instant::now(),
        }
    }

    pub fn finish(self, status: u16) {
        let elapsed = self.start.elapsed().as_millis() as u64;
        let icon = if status < 400 { "📤" } else { "💥" };
        let time = Local::now().format("%H:%M:%S");
        log_both(&format!(
            "[{}] {} {} ({}ms) {} {}",
            time, icon, status, elapsed, self.method, self.path
        ));
        append_request_log(
            self.method,
            &self.path,
            status,
            elapsed,
            self.model.as_deref(),
        );
    }
}

pub fn stream(event: &str, data: Option<&str>) {
    let time = Local::now().format("%H:%M:%S");
    match data {
        Some(d) => log_both(&format!("[{}] 🌊 [STREAM] {} {}", time, event, d)),
        None => log_both(&format!("[{}] 🌊 [STREAM] {}", time, event)),
    }
}
