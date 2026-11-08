//! Routing + catalog coverage for the OpenAI-compatible aggregators
//! dahl / atria / agnes / bai (bead
//! openproxy-9router-parity-v5104-sync-eidj.48).
//!
//! Upstream: `open-sse/providers/registry/{dahl,atria,agnes,bai}.js`
//! (commit 06112c1) — `inference.dahl.global`, `api.atria-asi.ai`,
//! `apihub.agnes-ai.com`, `api.b.ai`, all serving `/v1/chat/completions`.

use openproxy::core::executor::{provider_config_base_url, ClientPool, DefaultExecutor};
use openproxy::core::model::{parse_model, provider_catalog, resolve_provider_alias};
use std::sync::Arc;

/// (provider id, expected upstream chat URL, registry `aliases:` extras).
const PROVIDERS: &[(&str, &str, &[&str])] = &[
    (
        "dahl",
        "https://inference.dahl.global/v1/chat/completions",
        &["dahl-inference"],
    ),
    (
        "atria",
        "https://api.atria-asi.ai/v1/chat/completions",
        &["atria-asi"],
    ),
    (
        "agnes",
        "https://apihub.agnes-ai.com/v1/chat/completions",
        &["agnes-ai"],
    ),
    ("bai", "https://api.b.ai/v1/chat/completions", &["b-ai"]),
];

#[test]
fn each_aggregator_has_a_routing_entry_at_the_upstream_url() {
    for &(id, url, _) in PROVIDERS {
        assert_eq!(
            provider_config_base_url(id).as_deref(),
            Some(url),
            "{id} must route to its registry transport.baseUrl"
        );
        // Construction is the regression guard: a missing PROVIDER_CONFIGS
        // entry surfaces as UnsupportedProvider -> HTTP 500 on every request.
        DefaultExecutor::new(id.to_string(), Arc::new(ClientPool::new()), None)
            .unwrap_or_else(|e| panic!("{id} must construct a DefaultExecutor: {e}"));
    }
}

#[test]
fn registry_aliases_resolve_to_the_provider_id() {
    for &(id, _, aliases) in PROVIDERS {
        assert_eq!(resolve_provider_alias(id), id, "{id} must round-trip");
        for &alias in aliases {
            assert_eq!(
                resolve_provider_alias(alias),
                id,
                "registry alias {alias} must resolve to {id}"
            );
            let parsed = parse_model(&format!("{alias}/some-model"));
            assert_eq!(
                parsed.provider.as_deref(),
                Some(id),
                "{alias}/some-model must address {id}"
            );
        }
    }
}

#[test]
fn each_aggregator_is_registered_in_the_static_catalog() {
    let catalog = provider_catalog();
    for &(id, _, _) in PROVIDERS {
        let provider = catalog
            .provider_info(id)
            .unwrap_or_else(|| panic!("providers[] should contain {id}"));
        assert_eq!(provider.alias, id);
        assert!(
            provider.service_kinds.contains(&"llm".to_string()),
            "{id} must advertise the llm service kind"
        );
    }
}

#[test]
fn seeded_model_lists_match_the_registry() {
    let catalog = provider_catalog();

    let dahl = catalog.models_for_alias("dahl").expect("dahl models");
    let dahl_ids: Vec<&str> = dahl.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        dahl_ids,
        vec![
            "zai-org/GLM-5.3-Flash",
            "deepseek-ai/DeepSeek-V4-Flash-0731",
            "MiniMaxAI/MiniMax-M2.7"
        ]
    );

    let atria = catalog.models_for_alias("atria").expect("atria models");
    assert_eq!(atria.len(), 1);
    assert_eq!(atria[0].id, "Atria-Dawn-Preview");

    let agnes = catalog.models_for_alias("agnes").expect("agnes models");
    assert_eq!(agnes.len(), 4);
    assert!(agnes.iter().any(|m| m.id == "agnes-2.5-pro"));

    // bai ships no seed ids upstream — the live /v1/models is the source of
    // truth and passthroughModels accepts anything.
    assert!(catalog.models_for_alias("bai").is_some());
}
