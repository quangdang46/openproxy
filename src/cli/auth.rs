//! `openproxy auth` — manage credentials for remote server management.
//!
//! By default the CLI works against the local DB and needs no auth. The auth
//! subcommands only matter when you want to point this CLI at a *different*
//! `openproxy` server (e.g. on a teammate's box or a VPS):
//!
//! - `auth login --url <url> --api-key <key> [--profile <name>]`: saves the
//!   credentials as a profile in `~/.config/openproxy/config.toml`. The key
//!   is stored as plaintext TOML — see SECURITY note below. The CLI then
//!   activates the profile by setting `default_profile`.
//! - `auth logout [--profile <name>]`: deletes the named profile (or the
//!   current default).
//! - `auth whoami`: shows the active profile, its URL, and a masked key. In
//!   robot mode, also returns the resolved data dir and remote flag.
//!
//! SECURITY: storing API keys in plaintext is acceptable for personal dev
//! boxes — it's the same trust model as `~/.netrc` or `~/.aws/credentials`.
//! For shared machines, prefer setting `OPENPROXY_API_KEY` per-shell or
//! using a per-profile `api_key_env = "MY_VAR"` indirection.

use std::time::Duration;

use anyhow::Context;
use serde_json::json;

use crate::cli::config::{load_config_file, save_config_file, ResolvedConfig};
use crate::cli::output::{emit_error, emit_robot, humanln, mask_secret, OutputCtx};

pub struct LoginOptions {
    pub url: String,
    pub api_key: String,
    pub profile: Option<String>,
    /// If true, skip the live health probe and trust the user's input.
    pub no_verify: bool,
    /// If true, do not promote this profile to `default_profile`.
    pub no_activate: bool,
}

pub struct LogoutOptions {
    pub profile: Option<String>,
    /// If true, also clear `default_profile` when removing the current default.
    pub keep_default: bool,
}

/// `openproxy auth login` — persist credentials and (optionally) verify them.
pub async fn run_login(ctx: OutputCtx, opts: LoginOptions) -> anyhow::Result<i32> {
    let url = normalize_url(&opts.url);
    if !is_http_url(&url) {
        return Ok(emit_error(
            ctx,
            "validation",
            &format!("--url must start with http:// or https:// (got '{url}')"),
        )?);
    }
    if opts.api_key.trim().is_empty() {
        return Ok(emit_error(ctx, "validation", "--api-key cannot be empty")?);
    }

    // Optional connectivity probe so the user finds out about typos here
    // rather than on every subsequent command.
    let verified = if opts.no_verify {
        None
    } else {
        Some(probe_with_key(&url, &opts.api_key).await)
    };
    if let Some(false) = verified {
        return Ok(emit_error(
            ctx,
            "auth",
            &format!("could not authenticate against {url} (use --no-verify to skip this check)"),
        )?);
    }

    let profile_name = opts
        .profile
        .clone()
        .unwrap_or_else(|| default_profile_name(&url));

    let mut file = load_config_file().unwrap_or_default();
    let entry = file.profiles.entry(profile_name.clone()).or_default();
    entry.url = Some(url.clone());
    entry.api_key = Some(opts.api_key.clone());
    // Clear any indirection so the saved profile is self-contained.
    entry.api_key_env = None;

    if !opts.no_activate {
        file.default_profile = Some(profile_name.clone());
    }

    let path = save_config_file(&file).context("save updated config file")?;

    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.auth.login",
            json!({
                "profile": profile_name,
                "url": url,
                "config_file": path.display().to_string(),
                "verified": verified,
                "activated": !opts.no_activate,
                "masked_key": mask_secret(&opts.api_key),
            }),
        )?;
    } else {
        humanln(
            ctx,
            format!(
                "Saved profile '{profile_name}' -> {url} (key {})",
                mask_secret(&opts.api_key)
            ),
        );
        if !opts.no_activate {
            humanln(ctx, "Activated as default profile.");
        }
        humanln(ctx, format!("  config: {}", path.display()));
        match verified {
            Some(true) => humanln(ctx, "  verified: ok"),
            Some(false) => humanln(ctx, "  verified: FAIL"),
            None => humanln(ctx, "  verified: skipped"),
        }
    }
    Ok(0)
}

/// `openproxy auth logout` — remove a saved profile.
pub fn run_logout(ctx: OutputCtx, opts: LogoutOptions) -> anyhow::Result<i32> {
    let mut file = load_config_file().unwrap_or_default();
    let target = opts
        .profile
        .clone()
        .or_else(|| file.default_profile.clone());
    let Some(name) = target else {
        return Ok(emit_error(
            ctx,
            "not_found",
            "no profile to log out of (no --profile and no default_profile set)",
        )?);
    };

    if file.profiles.remove(&name).is_none() {
        return Ok(emit_error(
            ctx,
            "not_found",
            &format!("profile '{name}' not found in config"),
        )?);
    }

    if file.default_profile.as_deref() == Some(&name) && !opts.keep_default {
        file.default_profile = None;
    }

    let path = save_config_file(&file).context("save updated config file")?;

    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.auth.logout",
            json!({
                "profile": name,
                "config_file": path.display().to_string(),
            }),
        )?;
    } else {
        humanln(ctx, format!("Removed profile '{name}'"));
    }
    Ok(0)
}

/// `openproxy auth whoami` — describe the currently-resolved identity.
pub async fn run_whoami(ctx: OutputCtx, cfg: &ResolvedConfig, verify: bool) -> anyhow::Result<i32> {
    let file = load_config_file().unwrap_or_default();
    let profile = cfg.profile.clone().or(file.default_profile.clone());

    // Pick the live probe target: prefer the resolved remote URL if set,
    // otherwise fall back to the local default.
    let probe_target = cfg.remote_url.clone();

    let verified = if verify {
        match (probe_target.as_deref(), cfg.api_key.as_deref()) {
            (Some(url), Some(key)) => Some(probe_with_key(url, key).await),
            (Some(url), None) => Some(probe_health(url).await),
            _ => None,
        }
    } else {
        None
    };

    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.auth.whoami",
            json!({
                "profile": profile,
                "url": probe_target,
                "is_remote": cfg.is_remote(),
                "data_dir": cfg.data_dir.display().to_string(),
                "has_api_key": cfg.api_key.is_some(),
                "masked_key": cfg.api_key.as_deref().map(mask_secret),
                "verified": verified,
            }),
        )?;
    } else {
        humanln(ctx, "openproxy auth whoami:");
        humanln(
            ctx,
            format!("  profile: {}", profile.as_deref().unwrap_or("<none>")),
        );
        humanln(
            ctx,
            format!("  url: {}", probe_target.as_deref().unwrap_or("<local DB>")),
        );
        humanln(
            ctx,
            format!(
                "  api_key: {}",
                cfg.api_key
                    .as_deref()
                    .map(mask_secret)
                    .unwrap_or_else(|| "<none>".to_string())
            ),
        );
        humanln(ctx, format!("  data_dir: {}", cfg.data_dir.display()));
        match verified {
            Some(true) => humanln(ctx, "  verified: ok"),
            Some(false) => humanln(ctx, "  verified: FAIL"),
            None => {}
        }
    }
    Ok(if matches!(verified, Some(false)) {
        1
    } else {
        0
    })
}

/// `openproxy auth list` — show all configured profiles. Useful for agents
/// to discover what's available before picking one with `--profile`.
pub fn run_list(ctx: OutputCtx) -> anyhow::Result<i32> {
    let file = load_config_file().unwrap_or_default();
    let entries: Vec<_> = file
        .profiles
        .iter()
        .map(|(name, p)| {
            json!({
                "name": name,
                "url": p.url,
                "has_api_key": p.api_key.is_some() || p.api_key_env.is_some(),
                "data_dir": p.data_dir,
            })
        })
        .collect();

    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.auth.list",
            json!({
                "default_profile": file.default_profile,
                "profiles": entries,
            }),
        )?;
    } else {
        humanln(ctx, "Configured profiles:");
        if entries.is_empty() {
            humanln(ctx, "  (none)");
        } else {
            for entry in &entries {
                let name = entry["name"].as_str().unwrap_or("?");
                let url = entry["url"].as_str().unwrap_or("<no url>");
                let marker = if Some(name) == file.default_profile.as_deref() {
                    " (default)"
                } else {
                    ""
                };
                humanln(ctx, format!("  {name}{marker} -> {url}"));
            }
        }
    }
    Ok(0)
}

fn normalize_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

fn is_http_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

fn default_profile_name(url: &str) -> String {
    // Derive a reasonable default profile name from the URL host.
    // Examples:
    //   http://localhost:4623        -> "localhost"
    //   https://op.example.com:4623  -> "op-example-com"
    //   https://op.example.com       -> "op-example-com"
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host_part = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme);
    let host = host_part.split(':').next().unwrap_or(host_part);
    let host = host.trim();
    if host.is_empty() {
        return "remote".to_string();
    }
    host.replace('.', "-").to_lowercase()
}

async fn probe_health(url: &str) -> bool {
    let endpoint = format!("{}/api/health", url.trim_end_matches('/'));
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(1500))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    matches!(client.get(&endpoint).send().await, Ok(r) if r.status().is_success())
}

async fn probe_with_key(url: &str, key: &str) -> bool {
    // First make sure the server is up at all.
    if !probe_health(url).await {
        return false;
    }
    // Then try a private endpoint with the key. /api/providers requires auth
    // and is universally available so it's a fine smoke test.
    let endpoint = format!("{}/api/providers", url.trim_end_matches('/'));
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(2500))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    match client
        .get(&endpoint)
        .header("Authorization", format!("Bearer {key}"))
        .send()
        .await
    {
        // 200 = good; 401/403 = bad key; everything else = treat as bad to
        // surface the issue at login time.
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_name_strips_scheme_port_and_dots() {
        assert_eq!(default_profile_name("http://localhost:4623"), "localhost");
        assert_eq!(
            default_profile_name("https://op.example.com:4623"),
            "op-example-com"
        );
        assert_eq!(
            default_profile_name("https://OP.EXAMPLE.COM"),
            "op-example-com"
        );
        assert_eq!(default_profile_name("http://1.2.3.4:80/x"), "1-2-3-4");
    }

    #[test]
    fn normalize_url_strips_trailing_slash() {
        assert_eq!(normalize_url(" https://x/y/  "), "https://x/y");
        assert_eq!(normalize_url("http://x/"), "http://x");
    }

    #[test]
    fn is_http_url_checks_scheme() {
        assert!(is_http_url("http://x"));
        assert!(is_http_url("https://x"));
        assert!(!is_http_url("file:///x"));
        assert!(!is_http_url("x"));
    }

    /// Round-trip: login then logout against a temporary config file.
    #[tokio::test]
    async fn login_then_logout_round_trip() {
        let _g = crate::cli::test_lock::ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.toml");
        std::env::set_var("OPENPROXY_CONFIG", &cfg_path);

        run_login(
            OutputCtx::robot(),
            LoginOptions {
                url: "http://localhost:65535".into(),
                api_key: "test-key-1".into(),
                profile: Some("p1".into()),
                no_verify: true,
                no_activate: false,
            },
        )
        .await
        .unwrap();

        let file = load_config_file().unwrap();
        assert_eq!(file.default_profile.as_deref(), Some("p1"));
        let p = file.profiles.get("p1").unwrap();
        assert_eq!(p.url.as_deref(), Some("http://localhost:65535"));
        assert_eq!(p.api_key.as_deref(), Some("test-key-1"));

        run_logout(
            OutputCtx::robot(),
            LogoutOptions {
                profile: Some("p1".into()),
                keep_default: false,
            },
        )
        .unwrap();

        let file = load_config_file().unwrap();
        assert!(file.profiles.get("p1").is_none());
        assert!(file.default_profile.is_none());

        std::env::remove_var("OPENPROXY_CONFIG");
    }
}
