//! `openproxy schema list/show/example` — agent introspection.
//!
//! Lets agents discover what resources the CLI accepts and what shape they
//! take. This makes the CLI self-documenting — an agent can read these schemas
//! and generate valid payloads for `provider apply --from-file -` etc. without
//! browsing the dashboard.
//!
//! The schemas returned here are deliberately *minimal*: enough for a model to
//! generate a valid payload. They are NOT the authoritative serde shape —
//! that lives in `crate::types`. Treat these as a curated subset.

use serde_json::{json, Value};

use crate::cli::output::{emit_robot, humanln, OutputCtx};

const RESOURCES: &[&str] = &[
    "provider",
    "provider-node",
    "combo",
    "key",
    "pool",
    "settings",
    "custom-model",
    "model-alias",
];

pub fn run_list(ctx: OutputCtx) -> anyhow::Result<()> {
    let data = json!({
        "resources": RESOURCES,
    });
    if ctx.is_robot() {
        emit_robot("openproxy.v1.schema.list", data)?;
    } else {
        humanln(ctx, "Available resource schemas:");
        for r in RESOURCES {
            humanln(ctx, format!("  {r}"));
        }
    }
    Ok(())
}

pub fn run_show(ctx: OutputCtx, resource: &str) -> anyhow::Result<i32> {
    let Some(schema) = schema_for(resource) else {
        let exit = crate::cli::output::emit_error(
            ctx,
            "not_found",
            &format!("unknown resource '{resource}'. Try: openproxy schema list"),
        )?;
        return Ok(exit);
    };
    if ctx.is_robot() {
        emit_robot(
            &format!("openproxy.v1.schema.{}", normalize(resource)),
            schema,
        )?;
    } else {
        let pretty = serde_json::to_string_pretty(&schema).unwrap_or_default();
        println!("{pretty}");
    }
    Ok(0)
}

pub fn run_example(ctx: OutputCtx, resource: &str) -> anyhow::Result<i32> {
    let Some(example) = example_for(resource) else {
        let exit = crate::cli::output::emit_error(
            ctx,
            "not_found",
            &format!("unknown resource '{resource}'. Try: openproxy schema list"),
        )?;
        return Ok(exit);
    };
    if ctx.is_robot() {
        emit_robot(
            &format!("openproxy.v1.example.{}", normalize(resource)),
            example,
        )?;
    } else {
        let pretty = serde_json::to_string_pretty(&example).unwrap_or_default();
        println!("{pretty}");
    }
    Ok(0)
}

fn normalize(resource: &str) -> String {
    resource.replace('-', "_")
}

fn schema_for(resource: &str) -> Option<Value> {
    Some(match resource {
        "provider" => json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "ProviderConnection",
            "type": "object",
            "required": ["name", "provider"],
            "properties": {
                "id": {"type": "string", "description": "Auto-generated UUID if omitted"},
                "name": {"type": "string"},
                "provider": {"type": "string", "description": "Provider alias (openai, anthropic, ...) or node UUID"},
                "apiKey": {"type": "string"},
                "baseUrl": {"type": ["string", "null"]},
                "priority": {"type": "integer", "minimum": 0},
                "isActive": {"type": "boolean", "default": true},
                "defaultModel": {"type": ["string", "null"]},
                "providerSpecificData": {"type": "object"}
            }
        }),
        "provider-node" => json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "ProviderNode",
            "type": "object",
            "required": ["name", "type", "baseUrl"],
            "properties": {
                "id": {"type": "string"},
                "name": {"type": "string"},
                "type": {"type": "string", "enum": ["openai-compatible", "anthropic-compatible", "gemini-compatible"]},
                "baseUrl": {"type": "string", "format": "uri"}
            }
        }),
        "combo" => json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "Combo",
            "type": "object",
            "required": ["name", "models"],
            "properties": {
                "id": {"type": "string"},
                "name": {"type": "string"},
                "strategy": {"type": "string", "enum": ["fallback", "round-robin", "sticky-round-robin"], "default": "fallback"},
                "models": {"type": "array", "items": {"type": "string"}},
                "isActive": {"type": "boolean", "default": true}
            }
        }),
        "key" => json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "ApiKey",
            "type": "object",
            "required": ["name", "key"],
            "properties": {
                "id": {"type": "string"},
                "name": {"type": "string"},
                "key": {"type": "string"},
                "isActive": {"type": "boolean", "default": true}
            }
        }),
        "pool" => json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "ProxyPool",
            "type": "object",
            "required": ["name", "proxyUrl"],
            "properties": {
                "id": {"type": "string"},
                "name": {"type": "string"},
                "proxyUrl": {"type": "string"},
                "type": {"type": "string", "enum": ["http", "https", "socks5"], "default": "http"},
                "isActive": {"type": "boolean", "default": true},
                "strictProxy": {"type": "boolean", "default": false}
            }
        }),
        "settings" => json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "Settings",
            "type": "object",
            "properties": {
                "rtkEnabled": {"type": "boolean"},
                "cavemanEnabled": {"type": "boolean"},
                "cavemanLevel": {"type": "string", "enum": ["light", "medium", "heavy"]},
                "comboStrategy": {"type": "string"},
                "requireLogin": {"type": "boolean"},
                "observabilityEnabled": {"type": "boolean"},
                "outboundProxyEnabled": {"type": "boolean"},
                "outboundProxyUrl": {"type": "string"},
                "tunnelEnabled": {"type": "boolean"},
                "tunnelProvider": {"type": "string"}
            }
        }),
        "custom-model" => json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "CustomModel",
            "type": "object",
            "required": ["providerAlias", "id"],
            "properties": {
                "providerAlias": {"type": "string"},
                "id": {"type": "string"},
                "type": {"type": "string", "default": "chat"},
                "name": {"type": ["string", "null"]}
            }
        }),
        "model-alias" => json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "ModelAlias",
            "type": "object",
            "required": ["alias", "target"],
            "properties": {
                "alias": {"type": "string"},
                "target": {
                    "type": "object",
                    "required": ["provider", "model"],
                    "properties": {
                        "provider": {"type": "string"},
                        "model": {"type": "string"}
                    }
                }
            }
        }),
        _ => return None,
    })
}

fn example_for(resource: &str) -> Option<Value> {
    Some(match resource {
        "provider" => json!({
            "name": "openai-main",
            "provider": "openai",
            "apiKey": "sk-...",
            "priority": 10,
            "isActive": true
        }),
        "provider-node" => json!({
            "name": "my-custom-node",
            "type": "openai-compatible",
            "baseUrl": "https://api.example.com/v1"
        }),
        "combo" => json!({
            "name": "premium-coding",
            "strategy": "fallback",
            "models": ["openai/gpt-4o", "anthropic/claude-3-5-sonnet", "groq/llama-3.1-70b"]
        }),
        "key" => json!({
            "name": "ci-bot",
            "key": "op-...",
            "isActive": true
        }),
        "pool" => json!({
            "name": "us-east",
            "proxyUrl": "http://proxy.example.com:8080",
            "type": "http"
        }),
        "settings" => json!({
            "rtkEnabled": true,
            "cavemanEnabled": false,
            "cavemanLevel": "medium",
            "requireLogin": true
        }),
        "custom-model" => json!({
            "providerAlias": "openai",
            "id": "gpt-4o-2025-stub",
            "type": "chat",
            "name": "GPT-4o stub"
        }),
        "model-alias" => json!({
            "alias": "fast",
            "target": {
                "provider": "openai",
                "model": "gpt-4o-mini"
            }
        }),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_resource_has_schema_and_example() {
        for resource in RESOURCES {
            assert!(
                schema_for(resource).is_some(),
                "missing schema for {resource}"
            );
            assert!(
                example_for(resource).is_some(),
                "missing example for {resource}"
            );
        }
    }

    #[test]
    fn unknown_resource_returns_none() {
        assert!(schema_for("nope").is_none());
        assert!(example_for("nope").is_none());
    }
}
