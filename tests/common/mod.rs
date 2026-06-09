use std::collections::BTreeMap;
use std::sync::Arc;

use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::*;
use tempfile::tempdir;
#[allow(unused_imports)]
use wiremock::MockServer;

#[allow(dead_code)]
pub async fn boot_test_app() -> (axum::Router, AppState) {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![test_api_key()];
        state.provider_connections = vec![test_connection("openai")];
    })
    .await
    .expect("seed db");
    let state = AppState::new(db);
    (openproxy::build_app(state.clone()), state)
}

#[allow(dead_code)]
pub fn test_api_key() -> ApiKey {
    ApiKey {
        id: "test-key-id".into(),
        name: "Test".into(),
        key: "test-key".into(),
        machine_id: None,
        is_active: Some(true),
        created_at: None,
        extra: BTreeMap::new(),
    }
}

pub fn test_connection(provider: &str) -> ProviderConnection {
    ProviderConnection {
        id: format!("{provider}-conn"),
        provider: provider.to_string(),
        auth_type: "apikey".into(),
        name: Some(provider.into()),
        priority: Some(1),
        is_active: Some(true),
        created_at: None,
        updated_at: None,
        display_name: None,
        email: None,
        global_priority: None,
        default_model: None,
        access_token: None,
        refresh_token: None,
        expires_at: None,
        token_type: None,
        scope: None,
        id_token: None,
        project_id: None,
        api_key: Some("provider-key".into()),
        test_status: None,
        last_tested: None,
        last_error: None,
        last_error_at: None,
        rate_limited_until: None,
        expires_in: None,
        error_code: None,
        consecutive_use_count: None,
        backoff_level: None,
        consecutive_errors: None,
        proxy_url: None,
        proxy_label: None,
        use_connection_proxy: None,
        provider_specific_data: BTreeMap::new(),
        extra: BTreeMap::new(),
    }
}
