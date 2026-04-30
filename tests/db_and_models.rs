use std::collections::BTreeMap;
use std::sync::Arc;

use openproxy::core::model::{
    get_model_info, parse_model, resolve_model_alias_from_map, resolve_provider_alias,
    ModelRouteKind,
};
use openproxy::db::Db;
use openproxy::types::{
    ApiKey, AppDb, Combo, DailySummary, ModelAliasTarget, ProviderConnection, ProviderModelRef,
    ProviderNode, Settings, SummaryCounter, TokenUsage, UsageDb, UsageEntry,
};
use tempfile::tempdir;

#[test]
fn app_db_round_trips_through_serde() {
    let db = AppDb {
        provider_connections: vec![ProviderConnection {
            id: "conn-1".into(),
            provider: "openai".into(),
            auth_type: "apikey".into(),
            name: Some("Primary".into()),
            priority: Some(1),
            is_active: Some(true),
            created_at: Some("2026-01-01T00:00:00Z".into()),
            updated_at: Some("2026-01-01T00:00:00Z".into()),
            display_name: None,
            email: None,
            global_priority: None,
            default_model: Some("gpt-4.1".into()),
            access_token: None,
            refresh_token: None,
            expires_at: None,
            token_type: None,
            scope: None,
            id_token: None,
            project_id: None,
            api_key: Some("sk-test".into()),
            test_status: Some("ok".into()),
            last_tested: None,
            last_error: None,
            last_error_at: None,
            rate_limited_until: None,
            expires_in: None,
            error_code: None,
            consecutive_use_count: Some(0),
            provider_specific_data: BTreeMap::new(),
            extra: BTreeMap::new(),
        }],
        provider_nodes: vec![ProviderNode {
            id: "node-1".into(),
            r#type: "openai-compatible".into(),
            name: "OpenAI Node".into(),
            prefix: Some("custom".into()),
            api_type: Some("openai".into()),
            base_url: Some("https://example.com/v1".into()),
            created_at: Some("2026-01-01T00:00:00Z".into()),
            updated_at: Some("2026-01-01T00:00:00Z".into()),
            extra: BTreeMap::new(),
        }],
        combos: vec![Combo {
            id: "combo-1".into(),
            name: "writer".into(),
            models: vec![
                "openai/gpt-4.1".into(),
                "anthropic/claude-sonnet-4-5".into(),
            ],
            kind: Some("chat".into()),
            created_at: Some("2026-01-01T00:00:00Z".into()),
            updated_at: Some("2026-01-01T00:00:00Z".into()),
            extra: BTreeMap::new(),
        }],
        model_aliases: BTreeMap::from([
            (
                "fast".into(),
                ModelAliasTarget::Path("openai/gpt-4.1-mini".into()),
            ),
            (
                "precise".into(),
                ModelAliasTarget::Mapping(ProviderModelRef {
                    provider: "anthropic".into(),
                    model: "claude-opus-4-1".into(),
                    extra: BTreeMap::new(),
                }),
            ),
        ]),
        api_keys: vec![ApiKey {
            id: "key-1".into(),
            name: "Local".into(),
            key: "pk-test".into(),
            machine_id: Some("machine-1".into()),
            is_active: Some(true),
            created_at: Some("2026-01-01T00:00:00Z".into()),
            extra: BTreeMap::new(),
        }],
        settings: Settings::default(),
        pricing: BTreeMap::new(),
        ..AppDb::default()
    };

    let encoded = serde_json::to_value(&db).expect("encode app db");
    let decoded: AppDb = serde_json::from_value(encoded).expect("decode app db");

    assert_eq!(decoded, db);
}

#[test]
fn usage_db_round_trips_through_serde() {
    let usage = UsageDb {
        history: vec![UsageEntry {
            timestamp: Some("2026-01-01T00:00:00Z".into()),
            provider: Some("openai".into()),
            model: "gpt-4.1".into(),
            tokens: Some(TokenUsage {
                prompt_tokens: Some(10),
                input_tokens: None,
                completion_tokens: Some(20),
                output_tokens: None,
                total_tokens: Some(30),
                extra: BTreeMap::new(),
            }),
            connection_id: Some("conn-1".into()),
            api_key: Some("local-no-key".into()),
            endpoint: Some("/v1/chat/completions".into()),
            cost: Some(0.42),
            status: Some("ok".into()),
            extra: BTreeMap::new(),
        }],
        total_requests_lifetime: 1,
        daily_summary: BTreeMap::from([(
            "2026-01-01".into(),
            DailySummary {
                requests: 1,
                prompt_tokens: 10,
                completion_tokens: 20,
                cost: 0.42,
                by_provider: BTreeMap::from([(
                    "openai".into(),
                    SummaryCounter {
                        requests: 1,
                        prompt_tokens: 10,
                        completion_tokens: 20,
                        cost: 0.42,
                        raw_model: None,
                        provider: None,
                        api_key: None,
                        endpoint: None,
                        extra: BTreeMap::new(),
                    },
                )]),
                by_model: BTreeMap::new(),
                by_account: BTreeMap::new(),
                by_api_key: BTreeMap::new(),
                by_endpoint: BTreeMap::new(),
                extra: BTreeMap::new(),
            },
        )]),
        extra: BTreeMap::new(),
    };

    let encoded = serde_json::to_value(&usage).expect("encode usage db");
    let decoded: UsageDb = serde_json::from_value(encoded).expect("decode usage db");

    assert_eq!(decoded, usage);
}

#[tokio::test]
async fn db_loads_normalizes_and_persists_json_files() {
    let temp = tempdir().expect("tempdir");
    let db_json = temp.path().join("db.json");
    let usage_json = temp.path().join("usage.json");

    tokio::fs::write(
        &db_json,
        serde_json::to_vec_pretty(&serde_json::json!({
            "providerConnections": [],
            "apiKeys": [{ "id": "k1", "name": "Local", "key": "pk-test" }],
            "settings": { "outboundProxyUrl": "http://127.0.0.1:8080" }
        }))
        .expect("serialize db json"),
    )
    .await
    .expect("write db json");

    tokio::fs::write(
        &usage_json,
        serde_json::to_vec_pretty(&serde_json::json!({
            "history": [{ "model": "gpt-4.1" }]
        }))
        .expect("serialize usage json"),
    )
    .await
    .expect("write usage json");

    let db = Db::load_from(temp.path()).await.expect("load db");
    let snapshot = db.snapshot();
    let usage = db.usage_snapshot();

    assert!(snapshot.api_keys[0].is_active());
    assert!(snapshot.settings.outbound_proxy_enabled);
    assert_eq!(usage.total_requests_lifetime, 1);
    assert!(db.db_path.exists());
    assert!(db.usage_path.exists());

    db.update(|state| {
        state.model_aliases.insert(
            "draft".into(),
            ModelAliasTarget::Path("openai/gpt-4.1-mini".into()),
        );
    })
    .await
    .expect("update db");

    let reloaded = Db::load_from(temp.path()).await.expect("reload db");
    assert!(reloaded.snapshot().model_aliases.contains_key("draft"));
}

#[tokio::test]
async fn db_updates_are_serialized_and_snapshots_remain_lock_free() {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("load db"));
    let baseline = db.snapshot();

    let mut tasks = Vec::new();
    for index in 0..4 {
        let db = Arc::clone(&db);
        tasks.push(tokio::spawn(async move {
            db.update(|state| {
                state.combos.push(Combo {
                    id: format!("combo-{index}"),
                    name: format!("combo-{index}"),
                    models: vec![format!("openai/gpt-{index}")],
                    kind: None,
                    created_at: None,
                    updated_at: None,
                    extra: BTreeMap::new(),
                });
            })
            .await
            .expect("serialized update");
        }));
    }

    for task in tasks {
        task.await.expect("task joins");
    }

    assert!(baseline.combos.is_empty());
    assert_eq!(db.snapshot().combos.len(), 4);

    let reloaded = Db::load_from(temp.path()).await.expect("reload db");
    assert_eq!(reloaded.snapshot().combos.len(), 4);

    let temp_files = std::fs::read_dir(temp.path())
        .expect("read dir")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp"))
        .count();
    assert_eq!(temp_files, 0);
}

#[tokio::test]
async fn db_preserves_valid_sections_when_legacy_fields_are_null_or_invalid() {
    let temp = tempdir().expect("tempdir");
    let db_json = temp.path().join("db.json");
    let usage_json = temp.path().join("usage.json");

    tokio::fs::write(
        &db_json,
        serde_json::to_vec_pretty(&serde_json::json!({
            "providerConnections": [
                {
                    "id": "cookie-1",
                    "provider": "grok-web",
                    "authType": "cookie",
                    "name": "Web",
                    "isActive": true,
                    "providerSpecificData": { "cookie": "session=1" },
                    "unexpectedField": { "keep": true }
                }
            ],
            "providerNodes": null,
            "proxyPools": [
                {
                    "id": "pool-1",
                    "name": "Proxy",
                    "proxyUrl": "http://localhost:8080",
                    "strictProxy": true
                }
            ],
            "modelAliases": { "draft": { "provider": "openai", "model": "gpt-4.1-mini" } },
            "customModels": [{ "providerAlias": "openai", "id": "gpt-custom", "type": "llm", "name": "Custom" }],
            "mitmAlias": { "codex": { "chatgpt-4o-latest": "openai/gpt-4o" } },
            "combos": [{ "id": "combo-1", "name": "writer", "models": ["draft"] }],
            "apiKeys": [{ "id": "k1", "name": "Local", "key": "pk-test", "isActive": null }],
            "settings": { "requireLogin": null, "outboundProxyUrl": "http://127.0.0.1:8080" },
            "pricing": { "openai": { "gpt-4.1": { "input": 1.0, "output": 2.0 } } }
        }))
        .expect("serialize db json"),
    )
    .await
    .expect("write db json");

    tokio::fs::write(
        &usage_json,
        serde_json::to_vec_pretty(&serde_json::json!({
            "history": [
                {
                    "timestamp": "2026-01-01T00:00:00Z",
                    "provider": "openai",
                    "model": "gpt-4.1",
                    "tokens": { "prompt_tokens": 10, "completion_tokens": 20 },
                    "endpoint": "/v1/chat/completions"
                }
            ],
            "dailySummary": null
        }))
        .expect("serialize usage json"),
    )
    .await
    .expect("write usage json");

    let db = Db::load_from(temp.path()).await.expect("load db");
    let snapshot = db.snapshot();
    let usage = db.usage_snapshot();

    assert_eq!(snapshot.provider_connections.len(), 1);
    assert_eq!(snapshot.provider_connections[0].auth_type, "cookie");
    assert!(snapshot.provider_connections[0]
        .extra
        .contains_key("unexpectedField"));
    assert!(snapshot.provider_nodes.is_empty());
    assert_eq!(snapshot.proxy_pools.len(), 1);
    assert_eq!(snapshot.custom_models.len(), 1);
    assert_eq!(
        snapshot.mitm_alias["codex"]["chatgpt-4o-latest"],
        "openai/gpt-4o"
    );
    assert!(snapshot.api_keys[0].is_active());
    assert!(snapshot.settings.outbound_proxy_enabled);
    assert_eq!(usage.daily_summary["2026-01-01"].requests, 1);
    assert_eq!(usage.total_requests_lifetime, 1);
}

#[tokio::test]
async fn usage_updates_persist_and_migrate_daily_summary() {
    let temp = tempdir().expect("tempdir");
    let db = Db::load_from(temp.path()).await.expect("load db");

    db.update_usage(|usage| {
        usage.history.push(UsageEntry {
            timestamp: Some("2026-02-02T12:00:00Z".into()),
            provider: Some("anthropic".into()),
            model: "claude-sonnet-4-5".into(),
            tokens: Some(TokenUsage {
                prompt_tokens: Some(7),
                input_tokens: None,
                completion_tokens: Some(11),
                output_tokens: None,
                total_tokens: Some(18),
                extra: BTreeMap::new(),
            }),
            connection_id: Some("conn-9".into()),
            api_key: Some("key-9".into()),
            endpoint: Some("/v1/chat/completions".into()),
            cost: Some(0.21),
            status: Some("ok".into()),
            extra: BTreeMap::new(),
        });
    })
    .await
    .expect("update usage");

    let reloaded = Db::load_from(temp.path()).await.expect("reload");
    let usage = reloaded.usage_snapshot();
    let summary = &usage.daily_summary["2026-02-02"];

    assert_eq!(usage.total_requests_lifetime, 1);
    assert_eq!(summary.requests, 1);
    assert_eq!(summary.prompt_tokens, 7);
    assert_eq!(summary.completion_tokens, 11);
    assert_eq!(summary.by_provider["anthropic"].requests, 1);
    assert_eq!(
        summary.by_endpoint["/v1/chat/completions|claude-sonnet-4-5|anthropic"].requests,
        1
    );
}

#[test]
fn model_resolution_supports_aliases_nodes_and_combos() {
    let db = AppDb {
        provider_nodes: vec![ProviderNode {
            id: "node-openai".into(),
            r#type: "openai-compatible".into(),
            name: "Custom".into(),
            prefix: Some("custom".into()),
            api_type: Some("openai".into()),
            base_url: Some("https://example.com/v1".into()),
            created_at: None,
            updated_at: None,
            extra: BTreeMap::new(),
        }],
        model_aliases: BTreeMap::from([(
            "draft".into(),
            ModelAliasTarget::Path("cc/claude-sonnet-4-5".into()),
        )]),
        combos: vec![Combo {
            id: "combo-1".into(),
            name: "writer".into(),
            models: vec!["draft".into(), "openai/gpt-4.1".into()],
            kind: None,
            created_at: None,
            updated_at: None,
            extra: BTreeMap::new(),
        }],
        ..AppDb::default()
    };

    let parsed = parse_model("cc/claude-opus-4-7");
    assert_eq!(parsed.provider.as_deref(), Some("claude"));
    assert_eq!(parsed.provider_alias.as_deref(), Some("cc"));

    assert_eq!(resolve_provider_alias("kr"), "kiro");
    assert_eq!(resolve_provider_alias("custom"), "custom");

    let alias = resolve_model_alias_from_map("draft", &db.model_aliases).expect("resolve alias");
    assert_eq!(alias.provider, "claude");
    assert_eq!(alias.model, "claude-sonnet-4-5");

    let combo = get_model_info("writer", &db);
    assert_eq!(combo.route_kind, ModelRouteKind::Combo);
    assert_eq!(combo.provider, None);

    let explicit_combo = get_model_info("combo:writer", &db);
    assert_eq!(explicit_combo.route_kind, ModelRouteKind::Combo);
    assert_eq!(explicit_combo.model, "writer");

    let compatible = get_model_info("custom/gpt-4.1", &db);
    assert_eq!(compatible.provider.as_deref(), Some("node-openai"));
    assert_eq!(compatible.model, "gpt-4.1");

    let inferred = get_model_info("gpt-4.1-mini", &db);
    assert_eq!(inferred.provider.as_deref(), Some("openai"));

    let unknown_alias = get_model_info("mystery-model", &db);
    assert_eq!(unknown_alias.provider.as_deref(), Some("openai"));

    let empty = parse_model("");
    assert_eq!(empty.provider, None);
    assert_eq!(empty.model, None);

    let missing_model = parse_model("cc/");
    assert_eq!(missing_model.provider.as_deref(), Some("claude"));
    assert_eq!(missing_model.model.as_deref(), Some(""));
}
