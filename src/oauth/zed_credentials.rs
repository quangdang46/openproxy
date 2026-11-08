//! Zed IDE credential auto-import — port of 9router
//! `src/lib/oauth/utils/zedCredentials.js` (bead .72, upstream #4359).
//!
//! Reads the signed-in Zed IDE session from the OS secret store:
//! - Linux:   `secret-tool` (url=<credentials_url>, label `zed-github-account`)
//! - macOS:   Keychain internet password (`security find-internet-password`)
//! - Windows: Credential Manager target `zed:url=<credentials_url>`
//!
//! Also resolves `system_id` from Zed's local kv_store (`db.sqlite` under the
//! per-channel `db/` dir) so the imported connection carries the same
//! `x-zed-system-id` the IDE itself sends to cloud.zed.dev.

use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// JS ZED_DEFAULT_CREDENTIALS_URL.
pub const ZED_DEFAULT_CREDENTIALS_URL: &str = "https://zed.dev";
/// JS ZED_KEYRING_LABEL — Zed stores the GitHub-account secret under this label.
const ZED_KEYRING_LABEL: &str = "zed-github-account";

const SECRET_TOOL_TIMEOUT_MS: u64 = 8000;
const SECURITY_TIMEOUT_MS: u64 = 8000;
const POWERSHELL_TIMEOUT_MS: u64 = 15000;

/// Result of `read_zed_ide_credentials` (JS `{found, userId?, accessToken?,
/// systemId?, credentialsUrl?, error?}`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZedIdeCredentials {
    pub found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credentials_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn not_found(error: impl Into<String>, credentials_url: &str) -> ZedIdeCredentials {
    ZedIdeCredentials {
        found: false,
        user_id: None,
        access_token: None,
        system_id: None,
        credentials_url: Some(credentials_url.to_string()),
        error: Some(error.into()),
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// JS getZedSettingsPaths — one settings.json per platform.
fn zed_settings_paths() -> Vec<PathBuf> {
    let home = home_dir();
    match std::env::consts::OS {
        "macos" => vec![home.join("Library/Application Support/Zed/settings.json")],
        "windows" => {
            let local = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("AppData").join("Local"));
            vec![local.join("Zed").join("settings.json")]
        }
        _ => {
            let xdg = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"));
            vec![xdg.join("zed").join("settings.json")]
        }
    }
}

/// Resolve the credential URL Zed uses as the keyring/keychain key.
/// Honors settings.json `credentials_url` → `server_url`; defaults to
/// https://zed.dev (JS resolveZedCredentialsUrl).
pub fn resolve_zed_credentials_url() -> String {
    for settings_path in zed_settings_paths() {
        let Ok(raw) = std::fs::read_to_string(&settings_path) else {
            continue;
        };
        let Ok(settings) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let url = settings
            .get("credentials_url")
            .or_else(|| settings.get("server_url"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let Some(url) = url {
            return url.trim_end_matches('/').to_string();
        }
    }
    ZED_DEFAULT_CREDENTIALS_URL.to_string()
}

/// JS getZedGlobalDbPaths — candidate paths for Zed's global kv_store.
fn zed_global_db_paths() -> Vec<PathBuf> {
    let home = home_dir();
    let channels = ["0-global", "0-stable", "0-preview"];
    let base = match std::env::consts::OS {
        "macos" => home.join("Library/Application Support/Zed/db"),
        "windows" => {
            let local = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("AppData").join("Local"));
            local.join("Zed").join("db")
        }
        _ => {
            let xdg = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local").join("share"));
            xdg.join("zed").join("db")
        }
    };
    channels
        .iter()
        .map(|channel| base.join(channel).join("db.sqlite"))
        .collect()
}

/// Read `system_id` from Zed's local kv_store (JS readZedSystemId).
fn read_zed_system_id() -> Option<String> {
    for db_path in zed_global_db_paths() {
        if std::fs::File::open(&db_path).is_err() {
            continue;
        }
        if let Some(value) = query_kv_store(&db_path, "system_id") {
            let trimmed = value.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

/// JS queryKvStore — readonly SQLite read of `kv_store`. Zed's db is WAL-mode
/// and may be locked by the IDE; a readonly open still reads committed data.
fn query_kv_store(db_path: &PathBuf, key: &str) -> Option<String> {
    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    let value: rusqlite::types::Value = conn
        .query_row(
            "SELECT value FROM kv_store WHERE key = ?1 LIMIT 1",
            rusqlite::params![key],
            |row| row.get(0),
        )
        .ok()?;
    match value {
        rusqlite::types::Value::Text(s) => Some(s),
        rusqlite::types::Value::Blob(b) => String::from_utf8(b).ok(),
        rusqlite::types::Value::Integer(i) => Some(i.to_string()),
        rusqlite::types::Value::Real(f) => Some(f.to_string()),
        other => Some(format!("{other:?}")),
    }
}

struct CommandOutput {
    stdout: String,
    stderr: String,
    /// Process exit status when the command ran; `None` when it could not be
    /// spawned (JS `err.code === "ENOENT"`).
    ran: bool,
}

fn run_command(program: &str, args: &[&str], timeout_ms: u64) -> CommandOutput {
    use std::io::Read;
    let child = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        return CommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            ran: false,
        };
    };
    // Poll try_wait with a deadline (JS execFile `timeout`). Pipes are drained
    // after exit; the keyring/security outputs are a few hundred bytes, far
    // below the pipe buffer, so no reader threads are needed.
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => break,
        }
    }
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    if timed_out {
        stderr = format!("{stderr}\ncommand timed out after {timeout_ms}ms");
    }
    CommandOutput {
        stdout,
        stderr,
        ran: true,
    }
}

/// JS parseSecretToolSearch — attributes are often on stderr; callers
/// concatenate stderr+stdout. Format:
///   attribute.url = https://zed.dev
///   attribute.username = 123
///   [/60]
///   label = zed-github-account
///   secret = {...}
struct SecretToolItem {
    label: Option<String>,
    username: Option<String>,
    secret: Option<String>,
}

fn parse_secret_tool_search(text: &str) -> Vec<SecretToolItem> {
    let username = text
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == "attribute.username").then(|| value.trim().to_string())
        })
        .filter(|s| !s.is_empty());
    let label = text.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "label").then(|| value.trim().to_string())
    });
    let secret = text.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "secret").then(|| value.trim().to_string())
    });
    if username.is_none() && secret.is_none() && label.is_none() {
        return Vec::new();
    }
    vec![SecretToolItem {
        label,
        username,
        secret,
    }]
}

/// Linux: Zed (oo7) stores attributes url + username under label
/// "zed-github-account"; secret-tool prints attributes on stderr and
/// label/secret on stdout (JS readLinuxCredentials).
fn read_linux_credentials(credentials_url: &str) -> Result<(String, String), String> {
    let search = run_command(
        "secret-tool",
        &["search", "--all", "url", credentials_url],
        SECRET_TOOL_TIMEOUT_MS,
    );
    if !search.ran {
        return Err("secret-tool not found (install libsecret / secret-tools)".to_string());
    }
    // JS merges `${stderr}\n${stdout}` for parsing.
    let merged = format!("{}\n{}", search.stderr, search.stdout);
    let items = parse_secret_tool_search(&merged);
    let match_item = items
        .iter()
        .find(|item| {
            item.label.as_deref() == Some(ZED_KEYRING_LABEL)
                && item.username.is_some()
                && item.secret.is_some()
        })
        .or_else(|| {
            items
                .iter()
                .find(|item| item.username.is_some() && item.secret.is_some())
        });
    if let Some(item) = match_item {
        if let (Some(user), Some(secret)) = (&item.username, &item.secret) {
            return Ok((user.clone(), secret.clone()));
        }
    }

    // Fallback: `lookup` returns just the secret; a second `search` recovers
    // the username (JS lines 183-206).
    let lookup = run_command(
        "secret-tool",
        &["lookup", "url", credentials_url],
        SECRET_TOOL_TIMEOUT_MS,
    );
    if !lookup.ran {
        return Err("secret-tool not found (install libsecret / secret-tools)".to_string());
    }
    let access_token = lookup.stdout.trim().to_string();
    if access_token.is_empty() {
        return Err("Empty Zed keyring secret".to_string());
    }
    let search2 = run_command(
        "secret-tool",
        &["search", "--all", "url", credentials_url],
        SECRET_TOOL_TIMEOUT_MS,
    );
    let merged2 = format!("{}\n{}", search2.stderr, search2.stdout);
    let items2 = parse_secret_tool_search(&merged2);
    let user_id = items2
        .iter()
        .find_map(|item| item.username.clone())
        .ok_or_else(|| "Zed keyring entry missing username (user id)".to_string())?;
    Ok((user_id, access_token))
}

/// JS parseSecurityPassword — `security -g` prints the password to stderr as
/// `password: "..."` (with `\` and `"` escaped).
fn parse_security_password(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim_end();
        if let Some(rest) = trimmed.strip_prefix("password:") {
            let rest = rest.trim_start();
            if let Some(inner) = rest.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                // JS unescapeSecurityPassword: `\\(.)` → `$1`.
                let mut out = String::with_capacity(inner.len());
                let mut chars = inner.chars();
                while let Some(c) = chars.next() {
                    if c == '\\' {
                        if let Some(next) = chars.next() {
                            out.push(next);
                        }
                    } else {
                        out.push(c);
                    }
                }
                return Some(out);
            }
            // Empty password prints as `password: `
            if rest.is_empty() {
                return Some(String::new());
            }
        }
    }
    None
}

fn strip_url_scheme(url: &str) -> String {
    let no_scheme = url
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    no_scheme.trim_end_matches('/').to_string()
}

/// macOS: Zed stores an internet password with kSecAttrServer = full
/// credentials URL (JS readMacCredentials).
fn read_mac_credentials(credentials_url: &str) -> Result<(String, String), String> {
    let mut servers: Vec<String> = Vec::new();
    for candidate in [
        credentials_url.to_string(),
        strip_url_scheme(credentials_url),
        "zed.dev".to_string(),
    ] {
        if !candidate.is_empty() && !servers.contains(&candidate) {
            servers.push(candidate);
        }
    }
    let mut last_error: Option<String> = None;
    for server in servers {
        let output = run_command(
            "security",
            &["find-internet-password", "-s", &server, "-g"],
            SECURITY_TIMEOUT_MS,
        );
        if !output.ran {
            last_error = Some("security tool not found".to_string());
            continue;
        }
        // -g prints password to stderr; attributes to stdout.
        let combined = format!("{}\n{}", output.stdout, output.stderr);
        let password = parse_security_password(&combined);
        // JS: /"acct"<blob>="([^"]*)"/ then /"acct"<blob>=0x[0-9A-Fa-f]+\s+"([^"]*)"/
        let account = regex::Regex::new(r#""acct"<blob>="([^"]*)""#)
            .ok()
            .and_then(|re| {
                re.captures(&combined)
                    .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
            })
            .or_else(|| {
                regex::Regex::new(r#""acct"<blob>=0x[0-9A-Fa-f]+\s+"([^"]*)""#)
                    .ok()
                    .and_then(|re| {
                        re.captures(&combined)
                            .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
                    })
            });
        match (password, account) {
            (Some(password), Some(account)) if !password.is_empty() || !account.is_empty() => {
                if account.is_empty() {
                    last_error =
                        Some("Found Zed keychain password but missing account (user id)".into());
                    continue;
                }
                return Ok((account, password));
            }
            (Some(_), None) => {
                last_error =
                    Some("Found Zed keychain password but missing account (user id)".into());
            }
            _ => {
                last_error = Some(
                    output
                        .stderr
                        .lines()
                        .next()
                        .map(str::to_string)
                        .unwrap_or_else(|| "security find-internet-password failed".into()),
                );
            }
        }
    }
    Err(last_error.unwrap_or_else(|| "Zed credentials not found in macOS Keychain".into()))
}

/// Windows: Zed target name `zed:url=<credentials_url>` in Credential Manager
/// (JS readWindowsCredentials — CredRead via PowerShell).
fn read_windows_credentials(credentials_url: &str) -> Result<(String, String), String> {
    let target = format!("zed:url={credentials_url}");
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public class ZedCred {{
  [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
  public struct CREDENTIAL {{
    public uint Flags; public uint Type; public string TargetName; public string Comment;
    public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
    public uint CredentialBlobSize; public IntPtr CredentialBlob; public uint Persist;
    public uint AttributeCount; public IntPtr Attributes; public string TargetAlias; public string UserName;
  }}
  [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
  public static extern bool CredRead(string target, uint type, uint reservedFlag, out IntPtr credentialPtr);
  [DllImport("advapi32.dll", SetLastError = true)]
  public static extern void CredFree(IntPtr buffer);
  public static string Read(string target) {{
    IntPtr p;
    if (!CredRead(target, 1, 0, out p)) return null;
    try {{
      var c = (CREDENTIAL)Marshal.PtrToStructure(p, typeof(CREDENTIAL));
      string secret = "";
      if (c.CredentialBlob != IntPtr.Zero && c.CredentialBlobSize > 0) {{
        byte[] bytes = new byte[c.CredentialBlobSize];
        Marshal.Copy(c.CredentialBlob, bytes, 0, (int)c.CredentialBlobSize);
        secret = Encoding.UTF8.GetString(bytes);
      }}
      return (c.UserName ?? "") + "\n" + secret;
    }} finally {{ CredFree(p); }}
  }}
}}
"@
$r = [ZedCred]::Read('{}')
if ($null -eq $r) {{ exit 2 }}
Write-Output $r
"#,
        target.replace('\'', "''")
    );
    let output = run_command(
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command", &script],
        POWERSHELL_TIMEOUT_MS,
    );
    if !output.ran {
        return Err("powershell.exe not found".to_string());
    }
    let text = output.stdout.trim_start_matches('\u{FEFF}').to_string();
    let Some(nl) = text.find('\n') else {
        return Err("Malformed Windows credential payload".to_string());
    };
    let user_id = text[..nl].trim().to_string();
    let access_token = text[nl + 1..]
        .trim_end_matches('\n')
        .trim_end_matches('\r')
        .to_string();
    if user_id.is_empty() || access_token.is_empty() {
        return Err("Windows Credential Manager entry missing username or secret".to_string());
    }
    Ok((user_id, access_token))
}

/// Read Zed IDE session credentials from the OS secret store
/// (JS readZedIdeCredentials). Never panics; `found:false` carries `error`.
pub fn read_zed_ide_credentials() -> ZedIdeCredentials {
    let credentials_url = resolve_zed_credentials_url();
    let pair: Result<(String, String), String> = match std::env::consts::OS {
        "linux" => read_linux_credentials(&credentials_url),
        "macos" => read_mac_credentials(&credentials_url),
        "windows" => read_windows_credentials(&credentials_url),
        other => {
            return not_found(
                format!("Zed auto-import is not supported on platform {other}"),
                &credentials_url,
            )
        }
    };

    let (user_id, access_token) = match pair {
        Ok(pair) if !pair.0.is_empty() && !pair.1.is_empty() => pair,
        Ok(_) => {
            return not_found(
                "Zed IDE session not found in the system keyring. Sign in to Zed, then retry.",
                &credentials_url,
            )
        }
        Err(error) => return not_found(error, &credentials_url),
    };

    // system_id comes from the local kv_store; fall back to a fresh UUID like
    // JS `randomUUID()` so the connection still carries an id.
    let system_id = read_zed_system_id().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    ZedIdeCredentials {
        found: true,
        user_id: Some(user_id),
        access_token: Some(access_token),
        system_id: Some(system_id),
        credentials_url: Some(credentials_url),
        error: None,
    }
}
