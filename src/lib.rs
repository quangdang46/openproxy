pub mod cli;
pub mod core;
pub mod db;
pub mod oauth;
pub mod server;
pub mod types;

use axum::Router;

pub fn build_app(state: server::state::AppState) -> Router {
    server::api::routes()
        .merge(server::dashboard::routes())
        .with_state(state)
}
