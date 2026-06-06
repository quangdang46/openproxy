//! Catalog of MCP plugins openproxy can spawn under its stdio→SSE bridge,
//! plus the allowlist that gates which executables may be used for custom
//! plugins.
//!
//! Mirrors `src/shared/constants/coworkPlugins.js` from upstream 9router so
//! the dashboard and existing MCP clients see the same plugin names.
//!
//! Custom plugins are persisted on disk at
//! `${DATA_DIR}/mcp/customPlugins.json` and re-validated against the
//! allowlist on every load — `npx`/`uvx` injection from a writable settings
//! file is the obvious attack vector here.

use std::path::Path;
use std::sync::OnceLock;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// One spawnable stdio MCP plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDef {
    pub name: String,
    /// Display title for the dashboard.
    #[serde(default)]
    pub title: Option<String>,
    /// Short description for the dashboard.
    #[serde(default)]
    pub description: Option<String>,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Optional URL where users can install supporting browser/desktop
    /// extensions (e.g. browsermcp's Chrome extension).
    #[serde(default, rename = "extensionUrl")]
    pub extension_url: Option<String>,
    #[serde(default, rename = "toolNames")]
    pub tool_names: Vec<String>,
}

/// Built-in local stdio plugins. The dashboard surfaces these as the default
/// catalog; users can install/run them without editing settings on disk.
///
/// Currently mirrors upstream's `LOCAL_STDIO_PLUGINS` exactly (just
/// `browsermcp` for now). Adding new entries here is the only way to ship
/// "default" plugins without going through the customPlugins.json path.
pub fn local_stdio_plugins() -> Vec<PluginDef> {
    vec![PluginDef {
        name: "browsermcp".to_string(),
        title: Some("Browser MCP".to_string()),
        description: Some(
            "Control your running Chrome (requires Chrome extension)".to_string(),
        ),
        command: "npx".to_string(),
        args: vec![
            "-y".to_string(),
            "@browsermcp/mcp@latest".to_string(),
        ],
        extension_url: Some(
            "https://chromewebstore.google.com/detail/browser-mcp-automate-your/bjfgambnhccakkhmkepdoekmckoijdlc"
                .to_string(),
        ),
        tool_names: vec![
            "browser_navigate".to_string(),
            "browser_snapshot".to_string(),
            "browser_click".to_string(),
            "browser_type".to_string(),
            "browser_screenshot".to_string(),
            "browser_get_console_logs".to_string(),
            "browser_wait".to_string(),
            "browser_press_key".to_string(),
            "browser_go_back".to_string(),
            "browser_go_forward".to_string(),
        ],
    }]
}

/// Executables that may be spawned for custom stdio MCP plugins. Mirrors
/// upstream `ALLOWED_MCP_COMMANDS`. Anything else is rejected at
/// registration time.
pub const ALLOWED_MCP_COMMANDS: &[&str] =
    &["npx", "node", "uvx", "python", "python3", "bunx", "bun"];

/// True if `cmd` (after basename extraction) is on the allowlist.
pub fn is_allowed_command(cmd: &str) -> bool {
    let base = Path::new(cmd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(cmd);
    ALLOWED_MCP_COMMANDS.contains(&base)
}

/// In-memory cache of custom plugins. Survives across requests but resets on
/// process restart — `find_plugin` re-hydrates from
/// `${data_dir}/mcp/customPlugins.json` lazily.
fn custom_store() -> &'static RwLock<Vec<PluginDef>> {
    static STORE: OnceLock<RwLock<Vec<PluginDef>>> = OnceLock::new();
    STORE.get_or_init(|| RwLock::new(Vec::new()))
}

/// Path to the on-disk custom plugin catalog under `data_dir`.
pub fn custom_plugins_file(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("mcp").join("customPlugins.json")
}

/// Resolve a plugin by name. Checks (1) the in-memory custom store, then
/// (2) built-in `LOCAL_STDIO_PLUGINS`, then (3) lazily reads
/// `customPlugins.json` from disk (re-validating against the allowlist).
pub fn find_plugin(name: &str, data_dir: &Path) -> Option<PluginDef> {
    if let Some(found) = custom_store().read().iter().find(|p| p.name == name) {
        return Some(found.clone());
    }
    if let Some(found) = local_stdio_plugins().into_iter().find(|p| p.name == name) {
        return Some(found);
    }

    let path = custom_plugins_file(data_dir);
    let raw = std::fs::read_to_string(&path).ok()?;
    let list: Vec<PluginDef> = serde_json::from_str(&raw).ok()?;
    let def = list
        .into_iter()
        .find(|p| p.name == name && !p.command.is_empty() && is_allowed_command(&p.command))?;
    custom_store().write().push(def.clone());
    Some(def)
}

/// Register a custom plugin in the in-memory store. Refuses any command not
/// on the allowlist — same gate as upstream.
pub fn register_custom_plugin(def: PluginDef) -> Result<(), String> {
    if !is_allowed_command(&def.command) {
        return Err(format!(
            "Blocked: command '{}' not in MCP allowlist",
            def.command
        ));
    }
    let mut store = custom_store().write();
    if let Some(pos) = store.iter().position(|p| p.name == def.name) {
        store[pos] = def;
    } else {
        store.push(def);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn clear_custom_store_for_test() {
    custom_store().write().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn def(name: &str, cmd: &str) -> PluginDef {
        PluginDef {
            name: name.to_string(),
            title: None,
            description: None,
            command: cmd.to_string(),
            args: vec![],
            extension_url: None,
            tool_names: vec![],
        }
    }

    #[test]
    fn allowed_command_checks_basename() {
        assert!(is_allowed_command("npx"));
        assert!(is_allowed_command("/usr/local/bin/npx"));
        assert!(is_allowed_command("python3"));
        assert!(!is_allowed_command("curl"));
        assert!(!is_allowed_command("/bin/bash"));
        assert!(!is_allowed_command(""));
    }

    #[test]
    fn local_plugins_include_browsermcp() {
        let plugins = local_stdio_plugins();
        let browsermcp = plugins.iter().find(|p| p.name == "browsermcp").unwrap();
        assert_eq!(browsermcp.command, "npx");
        assert!(browsermcp.tool_names.iter().any(|t| t == "browser_click"));
    }

    #[test]
    fn register_rejects_disallowed_command() {
        clear_custom_store_for_test();
        let err = register_custom_plugin(def("evil", "rm")).unwrap_err();
        assert!(err.contains("not in MCP allowlist"));
        clear_custom_store_for_test();
    }

    #[test]
    fn register_accepts_allowed_command_and_overrides() {
        clear_custom_store_for_test();
        register_custom_plugin(def("foo", "npx")).unwrap();
        register_custom_plugin(def("foo", "uvx")).unwrap();
        let found = find_plugin("foo", Path::new("/nonexistent")).unwrap();
        assert_eq!(found.command, "uvx");
        clear_custom_store_for_test();
    }

    #[test]
    fn find_plugin_loads_from_disk() {
        clear_custom_store_for_test();
        let dir = tempfile::tempdir().unwrap();
        let data_dir: PathBuf = dir.path().to_path_buf();
        std::fs::create_dir_all(data_dir.join("mcp")).unwrap();
        let list = vec![def("disk-plug", "uvx")];
        std::fs::write(
            data_dir.join("mcp").join("customPlugins.json"),
            serde_json::to_string(&list).unwrap(),
        )
        .unwrap();

        let found = find_plugin("disk-plug", &data_dir).unwrap();
        assert_eq!(found.command, "uvx");

        // Disallowed command on disk must NOT be loaded even if the file is
        // present — defends against settings tampering.
        let bad = vec![def("nope", "bash")];
        std::fs::write(
            data_dir.join("mcp").join("customPlugins.json"),
            serde_json::to_string(&bad).unwrap(),
        )
        .unwrap();
        clear_custom_store_for_test();
        assert!(find_plugin("nope", &data_dir).is_none());
        clear_custom_store_for_test();
    }

    #[test]
    fn find_plugin_misses_when_not_found() {
        clear_custom_store_for_test();
        let dir = tempfile::tempdir().unwrap();
        assert!(find_plugin("does-not-exist", dir.path()).is_none());
    }
}
