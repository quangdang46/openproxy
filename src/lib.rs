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
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    server::api::routes()
        .merge(server::dashboard::routes())
        .layer(cors)
        .with_state(state)
}
