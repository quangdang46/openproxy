#![allow(
    dead_code,
    private_interfaces,
    unused_imports,
    unused_mut,
    unused_variables,
    clippy::result_large_err,
    clippy::if_same_then_else,
    clippy::too_many_arguments,
    clippy::field_reassign_with_default,
    clippy::should_implement_trait,
    clippy::unnecessary_filter_map,
    clippy::question_mark,
    clippy::type_complexity,
    clippy::await_holding_lock,
    clippy::unnecessary_get_then_check,
    clippy::ptr_arg,
    clippy::unnecessary_sort_by,
    clippy::redundant_locals
)]

pub mod cli;
pub mod core;
pub mod db;
pub mod oauth;
pub mod payload_rules;
pub mod server;
pub mod types;

use axum::Router;
use tower_http::cors::{Any, CorsLayer};

pub fn build_app(state: server::state::AppState) -> Router {
    use axum::middleware;

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    server::api::routes(state.clone())
        .merge(server::dashboard::routes())
        // Real-IP middleware: stamps the verified TCP peer IP as
        // `x-9r-real-ip` and strips client-supplied forwarding headers.
        .layer(middleware::from_fn(server::api::guard::real_ip_middleware))
        .layer(cors)
        .with_state(state)
}
