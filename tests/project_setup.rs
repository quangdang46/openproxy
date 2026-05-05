use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwap;
use aws_sigv4::{http_request::SigningSettings, SignatureVersion};
use axum::{body::Body, routing::get, Router};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use clap::Parser;
use http_body_util::Full;
use hyper::Request;
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::{connect::HttpConnector, Client};
use hyper_util::rt::TokioExecutor;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use openproxy::cli::Cli;
use openproxy::core::rtk::CompressionLevel;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tempfile::tempdir;
use tower_http::cors::CorsLayer;
use tracing_subscriber::EnvFilter;
use url::Url;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: usize,
}

#[test]
fn project_structure_matches_bead_layout() {
    for path in [
        "src/cli/mod.rs",
        "src/server/api/mod.rs",
        "src/server/dashboard/mod.rs",
        "src/server/auth/mod.rs",
        "src/core/proxy/mod.rs",
        "src/core/combo/mod.rs",
        "src/core/executor/mod.rs",
        "src/core/translator/mod.rs",
        "src/core/rtk/mod.rs",
        "src/core/auth/mod.rs",
        "src/core/model/mod.rs",
        "src/db/mod.rs",
        "src/oauth/mod.rs",
        "src/types/mod.rs",
    ] {
        assert!(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(path)
                .exists(),
            "missing required scaffold path: {path}"
        );
    }
}

#[test]
fn dependency_stack_smoke_test() {
    let _router: Router = Router::new()
        .route("/health", get(|| async { "ok" }))
        .layer(CorsLayer::permissive());

    let _request = Request::new(Body::from("ping"));
    let connector = HttpConnector::new();
    let _client = Client::builder(TokioExecutor::new()).build_http::<Body>();
    let https = HttpsConnectorBuilder::new()
        .with_native_roots()
        .expect("native roots load")
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .build();
    let _hyper_tls_client: Client<_, Full<Bytes>> =
        Client::builder(TokioExecutor::new()).build(https);

    let mut payload = br#"{"message":"hello"}"#.to_vec();
    let parsed = simd_json::to_owned_value(payload.as_mut_slice()).expect("simd-json parses");
    assert_eq!(parsed["message"], "hello");

    let claims = Claims {
        sub: "tester".into(),
        exp: 4_102_444_800,
    };
    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(b"secret"),
    )
    .expect("jwt encodes");
    let decoded = decode::<Claims>(
        &token,
        &DecodingKey::from_secret(b"secret"),
        &Validation::default(),
    )
    .expect("jwt decodes");
    assert_eq!(decoded.claims.sub, "tester");

    let snapshot = ArcSwap::from_pointee(json!({ "ok": true }));
    let stored = snapshot.load();
    assert_eq!(stored["ok"], true);

    let bytes = Bytes::from_static(b"payload");
    assert_eq!(bytes.len(), 7);

    let reqwest = reqwest::Client::builder()
        .cookie_store(true)
        .brotli(true)
        .gzip(true)
        .build()
        .expect("reqwest client builds");
    assert!(reqwest.get("https://example.com").build().is_ok());

    let parsed_url = Url::parse("https://example.com/v1/chat/completions?model=tester&stream=true")
        .expect("url parses");
    assert_eq!(parsed_url.host_str(), Some("example.com"));

    let parsed_query: Vec<(String, String)> =
        serde_urlencoded::from_str(parsed_url.query().expect("query"))
            .expect("query string decodes");
    assert!(parsed_query
        .iter()
        .any(|(key, value)| key == "model" && value == "tester"));

    let now = Utc::now();
    let round_trip = DateTime::parse_from_rfc3339(&now.to_rfc3339()).expect("chrono parses");
    assert_eq!(round_trip.with_timezone(&Utc).timestamp(), now.timestamp());

    let uuid = Uuid::new_v4();
    assert_ne!(uuid, Uuid::nil());

    let _signing_settings = SigningSettings::default();
    assert_eq!(SignatureVersion::V4.to_string(), "SigV4");

    let _subscriber = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("info"))
        .finish();

    let _ = connector;
    let _ = CompressionLevel::Lite;
}

#[test]
fn cli_parsing_supports_env_backed_flags() {
    let cli = Cli::try_parse_from([
        "openproxy",
        "--host",
        "127.0.0.1",
        "--port",
        "4623",
        "--log-filter",
        "debug",
    ])
    .expect("cli parses");

    assert_eq!(cli.host, "127.0.0.1");
    assert_eq!(cli.port, 4623);
    assert_eq!(cli.log_filter, "debug");
}

#[tokio::test]
async fn db_loader_creates_initial_files() {
    let temp = tempdir().expect("temp dir");
    let db = Db::load_from(temp.path()).await.expect("db loads");
    let snapshot = db.snapshot();

    assert!(db.db_path.exists());
    assert!(db.usage_path.exists());
    assert!(snapshot.provider_connections.is_empty());
    assert!(snapshot.settings.rtk_enabled);

    let state = AppState::new(Arc::new(db));
    assert!(state.db.db_path.starts_with(temp.path()));
}
