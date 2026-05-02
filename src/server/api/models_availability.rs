use axum::{
    routing::{get, post},
    Json, Router,
};

use crate::server::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/models/availability", get(get_availability))
        .route("/api/models/availability/check", post(check_availability))
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelAvailability {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub owned_by: String,
}

#[derive(Debug, serde::Serialize)]
pub struct ModelListResponse {
    pub object: String,
    pub data: Vec<ModelAvailability>,
}

// Hardcoded list of known available models
fn get_ollama_models() -> Vec<ModelAvailability> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    vec![
        ModelAvailability {
            id: "llama3".to_string(),
            object: "model".to_string(),
            created: now,
            owned_by: "meta".to_string(),
        },
        ModelAvailability {
            id: "llama3:70b".to_string(),
            object: "model".to_string(),
            created: now,
            owned_by: "meta".to_string(),
        },
        ModelAvailability {
            id: "mistral".to_string(),
            object: "model".to_string(),
            created: now,
            owned_by: "mistralai".to_string(),
        },
        ModelAvailability {
            id: "mixtral".to_string(),
            object: "model".to_string(),
            created: now,
            owned_by: "mistralai".to_string(),
        },
        ModelAvailability {
            id: "codellama".to_string(),
            object: "model".to_string(),
            created: now,
            owned_by: "meta".to_string(),
        },
        ModelAvailability {
            id: "phi3".to_string(),
            object: "model".to_string(),
            created: now,
            owned_by: "microsoft".to_string(),
        },
        ModelAvailability {
            id: "gemma".to_string(),
            object: "model".to_string(),
            created: now,
            owned_by: "google".to_string(),
        },
        ModelAvailability {
            id: "gemma2".to_string(),
            object: "model".to_string(),
            created: now,
            owned_by: "google".to_string(),
        },
        ModelAvailability {
            id: "qwen2".to_string(),
            object: "model".to_string(),
            created: now,
            owned_by: "qwen".to_string(),
        },
        ModelAvailability {
            id: "qwen2.5-coder".to_string(),
            object: "model".to_string(),
            created: now,
            owned_by: "qwen".to_string(),
        },
        ModelAvailability {
            id: "deepseek-coder".to_string(),
            object: "model".to_string(),
            created: now,
            owned_by: "deepseek".to_string(),
        },
        ModelAvailability {
            id: "codestral".to_string(),
            object: "model".to_string(),
            created: now,
            owned_by: "mistralai".to_string(),
        },
        ModelAvailability {
            id: "wizardlm2".to_string(),
            object: "model".to_string(),
            created: now,
            owned_by: "wizardlm".to_string(),
        },
    ]
}

async fn get_availability() -> Json<ModelListResponse> {
    Json(ModelListResponse {
        object: "list".to_string(),
        data: get_ollama_models(),
    })
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckAvailabilityRequest {
    pub model: String,
}

#[derive(Debug, serde::Serialize)]
pub struct CheckAvailabilityResponse {
    pub available: bool,
    pub model: String,
}

async fn check_availability(
    Json(req): Json<CheckAvailabilityRequest>,
) -> Json<CheckAvailabilityResponse> {
    let models = get_ollama_models();
    let available = models.iter().any(|m| m.id == req.model);

    Json(CheckAvailabilityResponse {
        available,
        model: req.model,
    })
}