//! Image generation provider adapters ported from
//! `open-sse/handlers/imageProviders/`.
//!
//! Each adapter implements the [`ImageAdapter`] trait. The public
//! [`get_image_adapter`] function maps a provider id (e.g. `"openai"`,
//! `"fal-ai"`, `"black-forest-labs"`) to a `&'static dyn ImageAdapter`.
//!
//! The handler in `core::media::image::handler` orchestrates one image
//! request: it calls `build_url` / `build_body` / `build_headers`, fires
//! the HTTP request, handles 401 retry-after-refresh, parses the
//! response, and emits an OpenAI-shaped response body.

mod base;
mod black_forest_labs;
mod cloudflare_ai;
mod codex;
mod comfyui;
mod fal_ai;
mod gemini;
pub mod handler;
mod huggingface;
mod nanobanana;
mod openai_compat;
mod runwayml;
mod sdwebui;
mod stability_ai;

pub use base::{
    sleep, size_to_aspect_ratio, url_to_base64, ImageAdapter, ImageRequest, ImageResponse,
    ParseContext, POLL_INTERVAL_MS, POLL_TIMEOUT_MS,
};
pub use handler::{handle_image_generation, ImageHandlerError};

/// Look up the image adapter for a provider id. Returns `None` if the
/// provider does not support image generation.
pub fn get_image_adapter(provider: &str) -> Option<&'static dyn ImageAdapter> {
    match provider {
        "openai" => Some(&openai_compat::OPENAI),
        "minimax" => Some(&openai_compat::MINIMAX),
        "openrouter" => Some(&openai_compat::OPENROUTER),
        "recraft" => Some(&openai_compat::RECRAFT),
        "gemini" => Some(&gemini::ADAPTER),
        "codex" => Some(&codex::ADAPTER),
        "sdwebui" => Some(&sdwebui::ADAPTER),
        "comfyui" => Some(&comfyui::ADAPTER),
        "huggingface" => Some(&huggingface::ADAPTER),
        "nanobanana" => Some(&nanobanana::ADAPTER),
        "fal-ai" => Some(&fal_ai::ADAPTER),
        "stability-ai" => Some(&stability_ai::ADAPTER),
        "black-forest-labs" => Some(&black_forest_labs::ADAPTER),
        "runwayml" => Some(&runwayml::ADAPTER),
        "cloudflare-ai" => Some(&cloudflare_ai::ADAPTER),
        _ => None,
    }
}

/// Returns true if `provider` supports image generation.
pub fn is_image_provider(provider: &str) -> bool {
    get_image_adapter(provider).is_some()
}
