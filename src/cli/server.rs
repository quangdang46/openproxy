//! `openproxy server` — manage the local server daemon lifecycle.
//!
//! Subcommands:
//! - `server start [--detach] [--host H] [--port P]`: start the API server.
//!   Without `--detach` we just run the server in the foreground (same as
//!   invoking `openproxy` with no subcommand). With `--detach` we re-exec
//!   ourselves as a fully detached child, write a PID file under `$DATA_DIR`,
//!   and probe the health endpoint before returning.
//! - `server stop`: read the PID file and send SIGTERM (Unix) or `kill` the
//!   process (Windows), waits up to 5s for graceful exit.
//! - `server status`: report whether a server is running for this `$DATA_DIR`
//!   and whether the local API is reachable.
//! - `server init [--force]`: create an empty `db.json` and emit the first
//!   admin API key (shown exactly once).

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context;
use serde_json::json;

use crate::cli::config::ResolvedConfig;
use crate::cli::output::{emit_error, emit_robot, humanln, OutputCtx};
use crate::db::Db;
use crate::types::ApiKey;

/// File name of the PID file written into `$DATA_DIR`.
pub const PID_FILE: &str = "openproxy.pid";
/// Sidecar file recording `<host>:<port>` of the running server so that
/// `server status` and `server stop` can probe the right endpoint without
/// asking the user.
const PORT_FILE: &str = "openproxy.endpoint";

#[derive(Debug)]
pub struct StartOptions {
    pub host: String,
    pub port: u16,
    pub detach: bool,
}

pub fn pid_file_path(data_dir: &Path) -> PathBuf {
    data_dir.join(PID_FILE)
}

/// Read the PID file, returning `None` if absent or unparseable.
pub fn read_pid(data_dir: &Path) -> Option<u32> {
    let p = pid_file_path(data_dir);
    let text = std::fs::read_to_string(&p).ok()?;
    text.trim().parse::<u32>().ok()
}

/// True iff a process with this PID is currently alive. On Unix we use
/// `kill(pid, 0)` which returns ESRCH if the process is gone.
pub fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: `kill` with signal 0 only checks existence; never modifies
        // the target process.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        // Best-effort fallback: assume alive if PID file exists.
        let _ = pid;
        true
    }
}

fn write_pid(data_dir: &Path, pid: u32) -> anyhow::Result<()> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("create data dir {}", data_dir.display()))?;
    std::fs::write(pid_file_path(data_dir), pid.to_string())
        .with_context(|| format!("write pid file in {}", data_dir.display()))?;
    Ok(())
}

fn write_endpoint(data_dir: &Path, host: &str, port: u16) -> anyhow::Result<()> {
    std::fs::write(data_dir.join(PORT_FILE), format!("{host}:{port}"))
        .with_context(|| format!("write endpoint file in {}", data_dir.display()))?;
    Ok(())
}

pub fn read_endpoint(data_dir: &Path) -> Option<(String, u16)> {
    let text = std::fs::read_to_string(data_dir.join(PORT_FILE)).ok()?;
    let trimmed = text.trim();
    let (host, port) = trimmed.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    Some((host.to_string(), port))
}

fn remove_pid(data_dir: &Path) {
    let _ = std::fs::remove_file(pid_file_path(data_dir));
    let _ = std::fs::remove_file(data_dir.join(PORT_FILE));
}

/// `openproxy server start`.
///
/// In foreground mode, returns `Ok(None)` to signal the caller (main.rs) to
/// continue with the in-process server boot. In detach mode, spawns a child
/// and returns `Ok(Some(exit_code))` so the caller exits.
pub async fn run_start(
    ctx: OutputCtx,
    cfg: &ResolvedConfig,
    opts: StartOptions,
) -> anyhow::Result<Option<i32>> {
    // Bail out early if another server appears to be running for this
    // DATA_DIR. This protects against accidental double-start.
    if let Some(existing) = read_pid(&cfg.data_dir) {
        if process_alive(existing) {
            let msg = format!(
                "openproxy already running (pid {existing}) for data dir {}",
                cfg.data_dir.display()
            );
            let exit = emit_error(ctx, "conflict", &msg)?;
            return Ok(Some(exit));
        }
        // Stale PID file: clean it up and continue.
        remove_pid(&cfg.data_dir);
    }

    if !opts.detach {
        // Foreground: let main.rs run the server loop. Write our own PID so
        // `server status` can find us.
        write_pid(&cfg.data_dir, std::process::id())?;
        write_endpoint(&cfg.data_dir, &opts.host, opts.port)?;
        if ctx.is_robot() {
            emit_robot(
                "openproxy.v1.server.start",
                json!({
                    "pid": std::process::id(),
                    "host": opts.host,
                    "port": opts.port,
                    "detached": false,
                    "data_dir": cfg.data_dir.display().to_string(),
                }),
            )?;
        } else {
            humanln(
                ctx,
                format!(
                    "Starting openproxy on {}:{} (pid {})",
                    opts.host,
                    opts.port,
                    std::process::id()
                ),
            );
        }
        return Ok(None);
    }

    // Detached: re-exec ourselves with the server defaults but no subcommand,
    // detach stdio, and probe the health endpoint to confirm it came up.
    let me = std::env::current_exe().context("locate current executable")?;
    let log_path = cfg.data_dir.join("openproxy.log");
    std::fs::create_dir_all(&cfg.data_dir).ok();
    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open log file {}", log_path.display()))?;
    let stderr = stdout.try_clone().context("clone log file handle")?;

    let mut cmd = std::process::Command::new(&me);
    cmd.arg("--no-open")
        .arg("--host")
        .arg(&opts.host)
        .arg("--port")
        .arg(opts.port.to_string())
        .arg("--data-dir")
        .arg(&cfg.data_dir);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(stdout);
    cmd.stderr(stderr);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: pre_exec is called after fork() in the child only. We use
        // `setsid` to disown the controlling terminal so the child survives
        // its parent. No allocation, no signals, no locks held.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    let child = cmd
        .spawn()
        .with_context(|| format!("spawn detached server from {}", me.display()))?;
    let child_pid = child.id();
    write_pid(&cfg.data_dir, child_pid)?;
    write_endpoint(&cfg.data_dir, &opts.host, opts.port)?;
    // Drop the Child handle without waiting; we don't want to reap.
    std::mem::forget(child);

    // Probe the health endpoint for up to ~5s. If it doesn't come up we still
    // succeed (the process is spawned) but warn.
    let probe_url = format!("http://127.0.0.1:{}/api/health", opts.port);
    let healthy = wait_for_health(&probe_url, Duration::from_secs(5)).await;

    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.server.start",
            json!({
                "pid": child_pid,
                "host": opts.host,
                "port": opts.port,
                "detached": true,
                "healthy": healthy,
                "data_dir": cfg.data_dir.display().to_string(),
                "log_file": log_path.display().to_string(),
            }),
        )?;
    } else {
        humanln(
            ctx,
            format!(
                "Started openproxy (pid {child_pid}) on {}:{} — logs: {}",
                opts.host,
                opts.port,
                log_path.display()
            ),
        );
        if !healthy {
            humanln(
                ctx,
                format!(
                    "  warning: health probe at {probe_url} did not respond in 5s; check the log"
                ),
            );
        }
    }
    Ok(Some(0))
}

/// `openproxy server stop`.
pub async fn run_stop(ctx: OutputCtx, cfg: &ResolvedConfig) -> anyhow::Result<i32> {
    let Some(pid) = read_pid(&cfg.data_dir) else {
        let msg = format!(
            "no openproxy.pid found in {} (server not started by this CLI?)",
            cfg.data_dir.display()
        );
        return Ok(emit_error(ctx, "not_found", &msg)?);
    };

    if !process_alive(pid) {
        remove_pid(&cfg.data_dir);
        if ctx.is_robot() {
            emit_robot(
                "openproxy.v1.server.stop",
                json!({"pid": pid, "result": "already_dead"}),
            )?;
        } else {
            humanln(ctx, format!("openproxy (pid {pid}) was not running"));
        }
        return Ok(0);
    }

    #[cfg(unix)]
    {
        // SAFETY: standard `kill(pid, SIGTERM)`; we own the choice of signal.
        let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if rc != 0 {
            let msg = format!(
                "kill(pid={pid}, SIGTERM) failed: {}",
                std::io::Error::last_os_error()
            );
            return Ok(emit_error(ctx, "other", &msg)?);
        }
    }

    // Wait up to 5s for the process to exit.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if !process_alive(pid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let stopped = !process_alive(pid);
    if stopped {
        remove_pid(&cfg.data_dir);
    }

    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.server.stop",
            json!({
                "pid": pid,
                "result": if stopped { "stopped" } else { "timeout" },
            }),
        )?;
    } else if stopped {
        humanln(ctx, format!("Stopped openproxy (pid {pid})"));
    } else {
        humanln(
            ctx,
            format!("Sent SIGTERM to pid {pid} but it is still alive after 5s"),
        );
    }
    Ok(if stopped { 0 } else { 1 })
}

/// `openproxy server status`.
pub async fn run_status(ctx: OutputCtx, cfg: &ResolvedConfig) -> anyhow::Result<i32> {
    let pid = read_pid(&cfg.data_dir);
    let alive = pid.map(process_alive).unwrap_or(false);

    // Probe order: explicit --url > recorded endpoint sidecar > default port.
    let probe_url = if let Some(url) = cfg.remote_url.clone() {
        url
    } else if let Some((host, port)) = read_endpoint(&cfg.data_dir) {
        // 0.0.0.0 / :: are bind addresses, not dial addresses.
        let dial_host = if host == "0.0.0.0" || host == "::" || host.is_empty() {
            "127.0.0.1".to_string()
        } else {
            host
        };
        format!("http://{dial_host}:{port}")
    } else {
        format!("http://127.0.0.1:{}", cfg.port)
    };
    let health_url = format!("{}/api/health", probe_url.trim_end_matches('/'));
    let reachable = probe_health(&health_url).await;

    let db_summary = match Db::load().await {
        Ok(db) => {
            let snap = db.snapshot();
            Some(json!({
                "providers": snap.provider_connections.len(),
                "keys": snap.api_keys.len(),
                "pools": snap.proxy_pools.len(),
                "combos": snap.combos.len(),
            }))
        }
        Err(_) => None,
    };

    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.server.status",
            json!({
                "pid": pid,
                "process_alive": alive,
                "reachable": reachable,
                "probe_url": health_url,
                "data_dir": cfg.data_dir.display().to_string(),
                "db": db_summary,
            }),
        )?;
    } else {
        humanln(ctx, "openproxy server:");
        match pid {
            Some(p) if alive => humanln(ctx, format!("  pid: {p} (alive)")),
            Some(p) => humanln(ctx, format!("  pid: {p} (stale)")),
            None => humanln(ctx, "  pid: none"),
        }
        humanln(
            ctx,
            format!(
                "  reachable: {} ({health_url})",
                if reachable { "yes" } else { "no" }
            ),
        );
        if let Some(db) = &db_summary {
            humanln(ctx, format!("  db: {db}"));
        }
    }
    Ok(if alive || reachable { 0 } else { 1 })
}

/// `openproxy server init`. Initializes an empty `db.json` and prints one
/// fresh admin API key. If a `db.json` already exists, behaviour depends on
/// what's inside it:
///
/// - apiKeys empty *and* providerConnections empty *and* combos empty: we
///   treat the data dir as effectively unprovisioned and append a fresh
///   admin key idempotently. This avoids the deadlock described in bug #4
///   where `openproxy doctor` (or just running the server once) creates an
///   empty `db.json`, after which `server init` refused to mint a key
///   without `--force` (which would also wipe future state).
/// - any of those is non-empty: we refuse to overwrite without `--force`.
pub async fn run_init(ctx: OutputCtx, cfg: &ResolvedConfig, force: bool) -> anyhow::Result<i32> {
    std::fs::create_dir_all(&cfg.data_dir)
        .with_context(|| format!("create data dir {}", cfg.data_dir.display()))?;

    let db_path = cfg.data_dir.join("db.json");
    let db_existed = db_path.exists();

    if db_existed && !force {
        // Inspect the existing DB. If it's an "empty shell" (no keys, no
        // providers, no combos) then we proceed with a non-destructive
        // mint of the admin key. Otherwise refuse to overwrite.
        match Db::load_from(&cfg.data_dir).await {
            Ok(db) => {
                let snap = db.snapshot();
                let truly_empty = snap.api_keys.is_empty()
                    && snap.provider_connections.is_empty()
                    && snap.combos.is_empty()
                    && snap.proxy_pools.is_empty();
                if !truly_empty {
                    let msg = format!(
                        "db.json already exists at {} (use --force to overwrite)",
                        db_path.display()
                    );
                    return Ok(emit_error(ctx, "conflict", &msg)?);
                }
            }
            Err(_) => {
                // db.json present but unparseable — refuse to clobber.
                let msg = format!(
                    "db.json at {} is unreadable (use --force to overwrite)",
                    db_path.display()
                );
                return Ok(emit_error(ctx, "conflict", &msg)?);
            }
        }
    }

    // Only touch a fresh empty document when we have no existing db (or
    // the user passed --force). For the "empty shell" idempotent path we
    // keep whatever serialized shape is already on disk and just append
    // the admin key via the normal `update` path.
    if !db_existed || force {
        let empty = serde_json::json!({
            "providerConnections": [],
            "providerNodes": [],
            "apiKeys": [],
            "proxyPools": [],
            "combos": [],
            "modelAliases": {},
            "modelAvailability": {},
            "settings": {}
        });
        let tmp = cfg.data_dir.join(".db.json.init");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&empty)?)
            .with_context(|| format!("write {}", tmp.display()))?;
        // Lock down permissions so the DB isn't world-readable on shared boxes.
        #[cfg(unix)]
        {
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, &db_path)
            .with_context(|| format!("install {}", db_path.display()))?;
    }

    // Now load the db and append a fresh admin key.
    let key_secret = generate_api_key();
    let key = ApiKey {
        id: uuid::Uuid::new_v4().to_string(),
        name: "admin".into(),
        key: key_secret.clone(),
        machine_id: None,
        is_active: Some(true),
        created_at: Some(chrono::Utc::now().to_rfc3339()),
        extra: std::collections::BTreeMap::new(),
    };

    let db = Db::load().await?;
    db.update(|d| d.api_keys.push(key.clone())).await?;

    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.server.init",
            json!({
                "data_dir": cfg.data_dir.display().to_string(),
                "db_path": db_path.display().to_string(),
                "admin_key": {
                    "id": key.id,
                    "name": key.name,
                    "key": key_secret,
                },
                "reused_existing_db": db_existed && !force,
            }),
        )?;
    } else {
        humanln(
            ctx,
            format!("Initialized openproxy at {}", cfg.data_dir.display()),
        );
        humanln(ctx, "");
        humanln(ctx, "Admin API key (save it now — shown only once):");
        humanln(ctx, format!("  {key_secret}"));
        humanln(ctx, "");
        humanln(
            ctx,
            "Start the server with: openproxy server start --detach",
        );
    }
    Ok(0)
}

fn generate_api_key() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("op-{hex}")
}

async fn probe_health(url: &str) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(800))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    matches!(client.get(url).send().await, Ok(r) if r.status().is_success())
}

async fn wait_for_health(url: &str, total: Duration) -> bool {
    let deadline = Instant::now() + total;
    while Instant::now() < deadline {
        if probe_health(url).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_file_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_pid(dir.path()).is_none());
        write_pid(dir.path(), 12345).unwrap();
        assert_eq!(read_pid(dir.path()), Some(12345));
        remove_pid(dir.path());
        assert!(read_pid(dir.path()).is_none());
    }

    #[test]
    fn generated_key_is_op_prefixed_and_long() {
        let k = generate_api_key();
        assert!(k.starts_with("op-"));
        // 24 random bytes -> 48 hex chars + "op-" -> 51 total.
        assert_eq!(k.len(), 51);
    }
}
