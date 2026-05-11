//! End-to-end test for `openproxy server start --detach` / `server stop`.
//!
//! Spawns the real binary, asserts the PID and endpoint sidecar files are
//! written, that the server answers on /api/health, and that stop tears
//! everything down. Skipped automatically if the binary is missing.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn locate_binary() -> Option<PathBuf> {
    // Honor the convention `CARGO_BIN_EXE_<name>` set during integration test
    // builds, with a fallback to `target/debug/openproxy` for `cargo test`
    // invocations that don't have the env var.
    if let Some(p) = option_env!("CARGO_BIN_EXE_openproxy") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    let cwd = std::env::current_dir().ok()?;
    for ancestor in cwd.ancestors() {
        let candidate = ancestor.join("target/debug/openproxy");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn next_free_port() -> u16 {
    // Bind to port 0 and let the OS pick a free one. We immediately drop the
    // listener — there is a tiny race against another process snatching the
    // port, but it's good enough for an integration test.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    port
}

fn wait_for_file(p: &Path, total: Duration) -> bool {
    let deadline = Instant::now() + total;
    while Instant::now() < deadline {
        if p.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn server_start_detach_writes_pid_and_endpoint_then_stop_cleans_up() {
    let Some(bin) = locate_binary() else {
        eprintln!("skipping: openproxy binary not found");
        return;
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let port = next_free_port();

    // First: init the data dir so the server has a db.json to load.
    let init = Command::new(&bin)
        .arg("--data-dir")
        .arg(dir.path())
        .arg("server")
        .arg("init")
        .arg("--robot")
        .output()
        .expect("run init");
    assert!(init.status.success(), "init failed: {init:?}");

    // Start detached.
    let start = Command::new(&bin)
        .arg("--data-dir")
        .arg(dir.path())
        .arg("server")
        .arg("start")
        .arg("--detach")
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .arg("--robot")
        .output()
        .expect("run start");
    assert!(
        start.status.success(),
        "detached start failed: stdout={} stderr={}",
        String::from_utf8_lossy(&start.stdout),
        String::from_utf8_lossy(&start.stderr)
    );

    let pid_file = dir.path().join("openproxy.pid");
    let endpoint_file = dir.path().join("openproxy.endpoint");
    assert!(
        wait_for_file(&pid_file, Duration::from_secs(3)),
        "pid file not created"
    );
    assert!(
        wait_for_file(&endpoint_file, Duration::from_secs(3)),
        "endpoint file not created"
    );

    let endpoint = std::fs::read_to_string(&endpoint_file).expect("read endpoint");
    assert_eq!(endpoint.trim(), format!("127.0.0.1:{port}"));

    // Status should report it alive and reachable.
    let status = Command::new(&bin)
        .arg("--data-dir")
        .arg(dir.path())
        .arg("server")
        .arg("status")
        .arg("--robot")
        .output()
        .expect("run status");
    assert!(status.status.success(), "status failed: {status:?}");
    let status_stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        status_stdout.contains("\"process_alive\":true"),
        "status did not report process_alive=true: {status_stdout}"
    );
    assert!(
        status_stdout.contains("\"reachable\":true"),
        "status did not report reachable=true: {status_stdout}"
    );

    // Stop.
    let stop = Command::new(&bin)
        .arg("--data-dir")
        .arg(dir.path())
        .arg("server")
        .arg("stop")
        .arg("--robot")
        .output()
        .expect("run stop");
    assert!(stop.status.success(), "stop failed: {stop:?}");
    let stop_stdout = String::from_utf8_lossy(&stop.stdout);
    assert!(
        stop_stdout.contains("\"result\":\"stopped\"")
            || stop_stdout.contains("\"result\":\"already_dead\""),
        "stop did not confirm shutdown: {stop_stdout}"
    );

    // PID and endpoint files should be gone.
    assert!(!pid_file.exists(), "pid file not cleaned up");
    assert!(!endpoint_file.exists(), "endpoint file not cleaned up");
}
