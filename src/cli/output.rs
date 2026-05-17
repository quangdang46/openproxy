//! CLI output formatting: human (default) vs robot (JSON envelope) vs NDJSON streams.
//!
//! Every CLI command should produce output through this module so that:
//! - Agents can rely on a stable, schema-versioned JSON envelope (`--robot`).
//! - Humans get readable, optionally colored output by default.
//! - Streaming commands emit one JSON object per line in `--robot` mode (NDJSON).

use std::io::{self, IsTerminal, Write};

use serde::Serialize;
use serde_json::{json, Value};

/// User-selectable output mode applied uniformly across commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputMode {
    /// Default human-readable output (auto-detect TTY for color).
    #[default]
    Human,
    /// `--robot`: stable JSON envelope to stdout. No banners, no color.
    Robot,
}

/// Color preference resolved from `--color` flag and TTY detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorMode {
    #[default]
    Auto,
    Always,
    Never,
}

impl ColorMode {
    pub fn enabled(self) -> bool {
        match self {
            ColorMode::Always => true,
            ColorMode::Never => false,
            ColorMode::Auto => io::stdout().is_terminal(),
        }
    }
}

/// Resolved per-invocation output settings, derived from global CLI flags.
#[derive(Debug, Clone, Copy, Default)]
pub struct OutputCtx {
    pub mode: OutputMode,
    pub color: ColorMode,
    pub quiet: bool,
}

impl OutputCtx {
    pub fn robot() -> Self {
        Self {
            mode: OutputMode::Robot,
            color: ColorMode::Never,
            quiet: false,
        }
    }

    pub fn is_robot(&self) -> bool {
        matches!(self.mode, OutputMode::Robot)
    }
}

/// Versioned JSON envelope for `--robot` output.
///
/// Every successful command emits exactly one envelope to stdout (or one per
/// line for streaming commands). Errors emit `RobotEnvelope::error(...)`.
#[derive(Debug, Clone, Serialize)]
pub struct RobotEnvelope {
    /// Stable schema identifier, e.g. `openproxy.v1.provider.list`.
    pub schema: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RobotError>,
    pub meta: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct RobotError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl RobotEnvelope {
    pub fn ok(schema: impl Into<String>, data: Value) -> Self {
        Self {
            schema: schema.into(),
            ok: true,
            data: Some(data),
            error: None,
            meta: json!({}),
        }
    }

    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            schema: "openproxy.v1.error".to_string(),
            ok: false,
            data: None,
            error: Some(RobotError {
                code: code.into(),
                message: message.into(),
                details: None,
            }),
            meta: json!({}),
        }
    }

    pub fn with_meta(mut self, meta: Value) -> Self {
        self.meta = meta;
        self
    }

    pub fn write_stdout(&self) -> io::Result<()> {
        let line = serde_json::to_string(self)
            .unwrap_or_else(|_| "{\"schema\":\"openproxy.v1.error\",\"ok\":false}".to_string());
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", line)?;
        stdout.flush()
    }
}

/// Convenience: emit a successful robot envelope (one JSON line).
pub fn emit_robot(schema: &str, data: Value) -> io::Result<()> {
    RobotEnvelope::ok(schema, data).write_stdout()
}

/// Convenience: emit an error envelope. Returns the suggested process exit code.
pub fn emit_error(ctx: OutputCtx, code: &str, message: &str) -> io::Result<i32> {
    if ctx.is_robot() {
        RobotEnvelope::error(code, message).write_stdout()?;
    } else if !ctx.quiet {
        eprintln!("error: {message}");
    }
    Ok(exit_code_for(code))
}

/// Map a logical error code to the conventional CLI exit code.
pub fn exit_code_for(code: &str) -> i32 {
    match code {
        "ok" => 0,
        "usage" => 2,
        "not_found" => 3,
        "conflict" => 4,
        "auth" => 5,
        "server_unreachable" | "network" => 6,
        "validation" => 7,
        _ => 1,
    }
}

/// Print a human-readable line, suppressed in robot/quiet modes.
///
/// Use this for status text that isn't part of the structured payload.
pub fn humanln(ctx: OutputCtx, line: impl AsRef<str>) {
    if ctx.is_robot() || ctx.quiet {
        return;
    }
    println!("{}", line.as_ref());
}

/// Mask a secret value for human display: keep first 4 + last 4 chars.
pub fn mask_secret(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() <= 8 {
        return "•".repeat(trimmed.len());
    }
    let head: String = trimmed.chars().take(4).collect();
    let tail: String = trimmed.chars().rev().take(4).collect::<String>();
    let tail: String = tail.chars().rev().collect();
    format!("{head}…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn robot_envelope_ok_serializes_minimal_shape() {
        let env = RobotEnvelope::ok("openproxy.v1.test", json!({"foo": 1}));
        let s = serde_json::to_string(&env).unwrap();
        assert!(s.contains("\"schema\":\"openproxy.v1.test\""));
        assert!(s.contains("\"ok\":true"));
        assert!(s.contains("\"foo\":1"));
        assert!(!s.contains("\"error\""));
    }

    #[test]
    fn robot_envelope_error_uses_error_schema() {
        let env = RobotEnvelope::error("not_found", "missing");
        let s = serde_json::to_string(&env).unwrap();
        assert!(s.contains("\"schema\":\"openproxy.v1.error\""));
        assert!(s.contains("\"ok\":false"));
        assert!(s.contains("\"code\":\"not_found\""));
    }

    #[test]
    fn exit_codes_are_stable() {
        assert_eq!(exit_code_for("ok"), 0);
        assert_eq!(exit_code_for("usage"), 2);
        assert_eq!(exit_code_for("not_found"), 3);
        assert_eq!(exit_code_for("conflict"), 4);
        assert_eq!(exit_code_for("auth"), 5);
        assert_eq!(exit_code_for("server_unreachable"), 6);
        assert_eq!(exit_code_for("validation"), 7);
        assert_eq!(exit_code_for("anything-else"), 1);
    }

    #[test]
    fn mask_secret_keeps_head_and_tail() {
        assert_eq!(mask_secret("sk-1234567890abcdef"), "sk-1…cdef");
        assert_eq!(mask_secret("short"), "•••••");
    }
}
