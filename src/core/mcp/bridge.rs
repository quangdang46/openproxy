//! Per-plugin stdio→broadcast bridge for MCP. One [`BridgeEntry`] per
//! plugin, shared across all SSE sessions of that plugin.
//!
//! Mirrors `getOrSpawn` / `registerSession` / `sendToChild` from upstream
//! 9router (`src/lib/mcp/stdioSseBridge.js`), but built on tokio:
//!
//!   * `tokio::process::Command` for the child process.
//!   * `tokio::sync::broadcast` for fan-out of filtered JSON-RPC frames to
//!     all subscribed SSE listeners.
//!   * `tokio::io::AsyncBufReadExt::lines()` for newline-delimited stdout
//!     framing.
//!
//! Lifecycle: the bridge is created on first SSE subscription (or message
//! POST) and torn down when the child exits or [`shutdown_plugin`] is
//! called. Slow SSE consumers are tolerated up to `BROADCAST_CAPACITY`
//! pending frames — beyond that the receiver gets `Lagged` and just resyncs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::OnceLock;

use parking_lot::Mutex as PlMutex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, Mutex};

use super::plugins::{find_plugin, PluginDef};
use super::smart_filter::filter_jsonrpc_frame;

/// Max number of frames buffered in the per-bridge broadcast channel before
/// laggy receivers start dropping. 256 is conservative — most MCP servers
/// emit a handful of frames per tool call.
pub const BROADCAST_CAPACITY: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("unknown plugin: {0}")]
    UnknownPlugin(String),
    #[error("failed to spawn `{cmd}`: {source}")]
    Spawn {
        cmd: String,
        #[source]
        source: std::io::Error,
    },
    #[error("child `{0}` has no piped stdio")]
    MissingStdio(String),
    #[error("failed to write to child stdin: {0}")]
    Write(#[source] std::io::Error),
    #[error("invalid JSON-RPC body: {0}")]
    InvalidJson(#[source] serde_json::Error),
    #[error("bridge not running for plugin `{0}`")]
    NotRunning(String),
}

/// One running plugin process plus its broadcast channel.
pub struct BridgeEntry {
    pub plugin_name: String,
    /// Writable end of the child's stdin. Held behind a tokio Mutex so the
    /// HTTP message handler can serialise frame writes without blocking the
    /// reactor.
    stdin: Mutex<Option<ChildStdin>>,
    /// Broadcast channel for filtered JSON-RPC frames (one item per line).
    sender: broadcast::Sender<String>,
    /// Retained so the bridge stays alive as long as `BridgeEntry` is held.
    /// The child is killed on shutdown via `kill_on_drop(true)`.
    #[allow(dead_code)]
    child: Mutex<Option<Child>>,
}

impl BridgeEntry {
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.sender.subscribe()
    }

    /// Forward a JSON-RPC frame to the child's stdin, with newline framing.
    pub async fn send_to_child(&self, frame: &serde_json::Value) -> Result<(), BridgeError> {
        let mut line = serde_json::to_string(frame).map_err(BridgeError::InvalidJson)?;
        line.push('\n');
        let mut guard = self.stdin.lock().await;
        let stdin = guard
            .as_mut()
            .ok_or_else(|| BridgeError::NotRunning(self.plugin_name.clone()))?;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(BridgeError::Write)?;
        stdin.flush().await.map_err(BridgeError::Write)?;
        Ok(())
    }
}

type BridgeMap = HashMap<String, Arc<BridgeEntry>>;

fn bridge_store() -> &'static PlMutex<BridgeMap> {
    static STORE: OnceLock<PlMutex<BridgeMap>> = OnceLock::new();
    STORE.get_or_init(|| PlMutex::new(HashMap::new()))
}

/// Return the running bridge for `name` if any.
pub fn get(name: &str) -> Option<Arc<BridgeEntry>> {
    bridge_store().lock().get(name).cloned()
}

/// Look up `name`'s [`PluginDef`] under `data_dir`, spawn the child if no
/// bridge is running yet, and return the live [`BridgeEntry`].
pub fn get_or_spawn(name: &str, data_dir: &Path) -> Result<Arc<BridgeEntry>, BridgeError> {
    if let Some(entry) = get(name) {
        return Ok(entry);
    }
    let plugin =
        find_plugin(name, data_dir).ok_or_else(|| BridgeError::UnknownPlugin(name.to_string()))?;
    spawn_bridge(plugin, data_dir.to_path_buf())
}

fn spawn_bridge(plugin: PluginDef, _data_dir: PathBuf) -> Result<Arc<BridgeEntry>, BridgeError> {
    let mut cmd = Command::new(&plugin.command);
    cmd.args(&plugin.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|e| BridgeError::Spawn {
        cmd: plugin.command.clone(),
        source: e,
    })?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| BridgeError::MissingStdio(plugin.name.clone()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| BridgeError::MissingStdio(plugin.name.clone()))?;
    let stderr = child.stderr.take();

    let (sender, _initial_receiver) = broadcast::channel::<String>(BROADCAST_CAPACITY);

    let entry = Arc::new(BridgeEntry {
        plugin_name: plugin.name.clone(),
        stdin: Mutex::new(Some(stdin)),
        sender: sender.clone(),
        child: Mutex::new(Some(child)),
    });

    {
        let mut store = bridge_store().lock();
        // Double-check under lock so we don't race with a concurrent caller.
        if let Some(existing) = store.get(&plugin.name) {
            return Ok(existing.clone());
        }
        store.insert(plugin.name.clone(), entry.clone());
    }

    // Stdout reader task: split on `\n`, filter, broadcast.
    {
        let sender = sender.clone();
        let name = plugin.name.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            loop {
                match reader.next_line().await {
                    Ok(Some(line)) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        let filtered = filter_jsonrpc_frame(trimmed);
                        let _ = sender.send(filtered);
                    }
                    Ok(None) => break,
                    Err(err) => {
                        tracing::warn!(
                            target: "openproxy::mcp",
                            plugin = %name,
                            error = %err,
                            "stdout read error; tearing down bridge"
                        );
                        break;
                    }
                }
            }
            shutdown_plugin(&name).await;
        });
    }

    // Stderr drain task: log lines as warnings, mirroring upstream's
    // `console.log(...)`.
    if let Some(stderr) = stderr {
        let name = plugin.name.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                tracing::warn!(target: "openproxy::mcp", plugin = %name, "{line}");
            }
        });
    }

    Ok(entry)
}

/// Tear down the bridge for `name` (closes stdin, kills child, removes from
/// the registry). Idempotent.
pub async fn shutdown_plugin(name: &str) {
    let removed = bridge_store().lock().remove(name);
    if let Some(entry) = removed {
        {
            let mut guard = entry.stdin.lock().await;
            *guard = None;
        }
        let mut guard = entry.child.lock().await;
        if let Some(mut child) = guard.take() {
            let _ = child.start_kill();
            // Don't await child.wait() here — kill_on_drop on the Child
            // dropped from `entry`'s Mutex handles cleanup async-ly.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::mcp::plugins::{
        clear_custom_store_for_test, register_custom_plugin, PluginDef,
    };
    use std::time::Duration;
    use tempfile::tempdir;

    fn cat_plugin(name: &str) -> PluginDef {
        // `cat` is not on the allowlist, so we have to register via test-only
        // bypass. But upstream parity demands we use an allowlisted command;
        // wrap `cat` through node -e for tests so we exercise the spawn path
        // without coupling to npm.
        PluginDef {
            name: name.to_string(),
            title: None,
            description: None,
            command: "node".to_string(),
            args: vec![
                "-e".to_string(),
                // Echo each stdin line back inside a JSON-RPC frame so the
                // bridge has something to broadcast and we exercise the
                // filter path.
                r#"process.stdin.setEncoding('utf8');let buf='';process.stdin.on('data',c=>{buf+=c;let i;while((i=buf.indexOf('\n'))>=0){const raw=buf.slice(0,i);buf=buf.slice(i+1);try{const m=JSON.parse(raw);process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:m.id,result:{content:[{type:'text',text:'echo:'+(m.params||'')}]}})+'\n');}catch{}}});"#.to_string(),
            ],
            extension_url: None,
            tool_names: vec![],
        }
    }

    #[tokio::test]
    async fn get_or_spawn_returns_error_for_unknown_plugin() {
        let dir = tempdir().unwrap();
        let err = match get_or_spawn("nope-nonexistent", dir.path()) {
            Ok(_) => panic!("expected error"),
            Err(e) => e,
        };
        assert!(matches!(err, BridgeError::UnknownPlugin(_)));
    }

    #[tokio::test]
    async fn full_round_trip_via_node_echo() {
        // Skip the test gracefully if no node binary is present in PATH.
        if which_node().is_none() {
            eprintln!("skipping: node not on PATH");
            return;
        }
        clear_custom_store_for_test();
        let dir = tempdir().unwrap();
        register_custom_plugin(cat_plugin("echo-test")).unwrap();

        let entry = get_or_spawn("echo-test", dir.path()).expect("spawn");
        let mut rx = entry.subscribe();

        let req = serde_json::json!({"id": 7, "params": "hi"});
        entry.send_to_child(&req).await.unwrap();

        let line = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out")
            .expect("recv");

        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["id"], 7);
        assert_eq!(parsed["result"]["content"][0]["text"], "echo:hi");

        shutdown_plugin("echo-test").await;
        clear_custom_store_for_test();
    }

    #[tokio::test]
    async fn shutdown_plugin_is_idempotent() {
        shutdown_plugin("does-not-exist").await;
        shutdown_plugin("does-not-exist").await; // still fine
    }

    fn which_node() -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        for entry in std::env::split_paths(&path) {
            let candidate = entry.join("node");
            if candidate.exists() {
                return Some(candidate);
            }
        }
        None
    }
}
