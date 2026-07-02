//! A2A (Agent-to-Agent) protocol implementation for OpenProxy.
//!
//! Implements the [A2A protocol](https://github.com/google/A2A) for agent
//! discovery and task-based communication.  A2A lets agents discover one
//! another's capabilities via an Agent Card (served at `/.well-known/agent.json`)
//! and exchange tasks through a JSON-RPC-style Task API.
//!
//! Protocol endpoints:
//!   * `GET  /.well-known/agent.json`     — Agent Card (discovery)
//!   * `POST /api/a2a/tasks/send`          — Create and execute a task
//!   * `GET  /api/a2a/tasks/{id}`          — Get task status + result
//!   * `POST /api/a2a/tasks/{id}/cancel`   — Cancel a running task
//!   * `GET  /api/a2a/tasks/{id}/stream`   — SSE stream of task events

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::types::AppDb;

// ─── Agent Card ────────────────────────────────────────────────────────────

/// A2A Agent Card — describes an agent's capabilities.
/// Served at `/.well-known/agent.json`.
#[derive(Debug, Clone, Serialize)]
pub struct AgentCard {
    pub name: String,
    pub description: String,
    pub url: String,
    pub version: String,
    pub capabilities: AgentCapabilities,
    pub skills: Vec<AgentSkill>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authentication: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentCapabilities {
    pub streaming: bool,
    pub push_notifications: bool,
    pub state_transition_history: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
}

impl AgentCard {
    /// Build the default OpenProxy Agent Card.
    pub fn new(base_url: &str) -> Self {
        let api_base = base_url.trim_end_matches('/');
        Self {
            name: "OpenProxy".to_string(),
            description: "OpenProxy — AI proxy/router for multi-provider model access, routing, and configuration management".to_string(),
            url: format!("{api_base}/api/a2a"),
            version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: AgentCapabilities {
                streaming: true,
                push_notifications: false,
                state_transition_history: true,
            },
            skills: vec![
                AgentSkill {
                    id: "provider_management".to_string(),
                    name: "Provider Management".to_string(),
                    description: "List, create, test, and delete AI provider connections".to_string(),
                    tags: vec!["provider".to_string(), "ai".to_string(), "configuration".to_string()],
                    input_schema: None,
                    output_schema: None,
                },
                AgentSkill {
                    id: "key_management".to_string(),
                    name: "API Key Management".to_string(),
                    description: "Create, list, and delete API keys".to_string(),
                    tags: vec!["key".to_string(), "auth".to_string(), "configuration".to_string()],
                    input_schema: None,
                    output_schema: None,
                },
                AgentSkill {
                    id: "model_routing".to_string(),
                    name: "Model Routing".to_string(),
                    description: "Configure model combos and routing policies for AI model access".to_string(),
                    tags: vec!["model".to_string(), "routing".to_string(), "ai".to_string()],
                    input_schema: None,
                    output_schema: None,
                },
                AgentSkill {
                    id: "proxy_management".to_string(),
                    name: "Proxy Pool Management".to_string(),
                    description: "Create, list, and delete proxy pools for outbound connections".to_string(),
                    tags: vec!["proxy".to_string(), "network".to_string(), "configuration".to_string()],
                    input_schema: None,
                    output_schema: None,
                },
                AgentSkill {
                    id: "node_management".to_string(),
                    name: "Provider Node Management".to_string(),
                    description: "List provider nodes (compute endpoints)".to_string(),
                    tags: vec!["node".to_string(), "provider".to_string(), "configuration".to_string()],
                    input_schema: None,
                    output_schema: None,
                },
                AgentSkill {
                    id: "health_monitoring".to_string(),
                    name: "Health Monitoring".to_string(),
                    description: "Check server health, status, and version information".to_string(),
                    tags: vec!["health".to_string(), "monitoring".to_string(), "status".to_string()],
                    input_schema: None,
                    output_schema: None,
                },
            ],
            authentication: None,
        }
    }
}

// ─── Task Protocol ─────────────────────────────────────────────────────────

/// A2A task states.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Submitted,
    Working,
    InputRequired,
    Completed,
    Canceled,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub state: TaskState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<TaskResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<TaskError>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// The skill ID this task targets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskResult {
    pub parts: Vec<TaskPart>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_history: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskPart {
    #[serde(rename = "type")]
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskError {
    pub code: i32,
    pub message: String,
}

/// A2A task send request.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSendRequest {
    pub id: String,
    pub session_id: Option<String>,
    pub message: TaskMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskMessage {
    pub parts: Vec<TaskPart>,
    pub role: Option<String>,
}

impl Task {
    fn new(id: String, skill_id: Option<String>) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id,
            state: TaskState::Submitted,
            result: None,
            error: None,
            created_at: now.clone(),
            updated_at: Some(now),
            skill_id,
        }
    }
}

/// In-memory task store. Shared across all A2A API handlers.
#[derive(Clone)]
pub struct TaskStore {
    tasks: Arc<RwLock<HashMap<String, Task>>>,
}

impl TaskStore {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn insert(&self, task: Task) {
        self.tasks.write().await.insert(task.id.clone(), task);
    }

    pub async fn get(&self, id: &str) -> Option<Task> {
        self.tasks.read().await.get(id).cloned()
    }

    pub async fn update_state(&self, id: &str, state: TaskState) -> Option<Task> {
        let mut guard = self.tasks.write().await;
        if let Some(task) = guard.get_mut(id) {
            task.state = state;
            task.updated_at = Some(chrono::Utc::now().to_rfc3339());
            return Some(task.clone());
        }
        None
    }

    pub async fn update_result(&self, id: &str, result: TaskResult) -> Option<Task> {
        let mut guard = self.tasks.write().await;
        if let Some(task) = guard.get_mut(id) {
            task.state = TaskState::Completed;
            task.result = Some(result);
            task.updated_at = Some(chrono::Utc::now().to_rfc3339());
            return Some(task.clone());
        }
        None
    }

    pub async fn update_error(&self, id: &str, error: TaskError) -> Option<Task> {
        let mut guard = self.tasks.write().await;
        if let Some(task) = guard.get_mut(id) {
            task.state = TaskState::Failed;
            task.error = Some(error);
            task.updated_at = Some(chrono::Utc::now().to_rfc3339());
            return Some(task.clone());
        }
        None
    }
}

impl Default for TaskStore {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Task dispatch ─────────────────────────────────────────────────────────

/// Dispatch an incoming A2A task by matching its message parts to
/// internal OpenProxy operations.
pub async fn dispatch_task(
    store: &TaskStore,
    request: TaskSendRequest,
    _state: &crate::server::state::AppState,
) -> Task {
    let id = request.id.clone();
    let skill_id = request
        .metadata
        .as_ref()
        .and_then(|m| m.get("skillId"))
        .and_then(Value::as_str)
        .map(String::from);

    store.insert(Task::new(id.clone(), skill_id.clone())).await;
    let _ = store.update_state(&id, TaskState::Working).await;

    let parts_text: String = request
        .message
        .parts
        .iter()
        .filter_map(|p| p.text.as_deref())
        .collect::<Vec<_>>()
        .join("\n");

    let result_parts = match skill_id.as_deref() {
        Some("provider_management") => handle_provider_skill(&parts_text, _state),
        Some("key_management") => handle_key_skill(&parts_text),
        Some("model_routing") => handle_model_skill(&parts_text),
        Some("proxy_management") => handle_proxy_skill(&parts_text),
        Some("health_monitoring") => handle_health_skill(),
        _ => handle_generic_task(&parts_text, _state),
    };

    let task_result = TaskResult {
        parts: result_parts,
        is_history: Some(false),
    };

    store
        .update_result(&id, task_result)
        .await
        .unwrap_or_else(|| Task {
            id,
            state: TaskState::Unknown,
            result: None,
            error: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: None,
            skill_id,
        })
}

fn handle_provider_skill(text: &str, state: &crate::server::state::AppState) -> Vec<TaskPart> {
    let snap = state.db.snapshot();
    let providers_json = serde_json::to_string_pretty(&snap.provider_connections)
        .unwrap_or_else(|_| "[]".to_string());
    vec![
        TaskPart {
            r#type: "text".to_string(),
            text: Some(format!("OpenProxy Provider Management\n\nAvailable connections:\n{providers_json}\n\nQuery: {text}")),
            metadata: None,
        },
    ]
}

fn handle_key_skill(text: &str) -> Vec<TaskPart> {
    vec![
        TaskPart {
            r#type: "text".to_string(),
            text: Some(format!("Key management request: {text}\n\nUse tools/key_list, tools/key_create, or tools/key_delete via MCP to manage API keys.")),
            metadata: None,
        },
    ]
}

fn handle_model_skill(text: &str) -> Vec<TaskPart> {
    vec![
        TaskPart {
            r#type: "text".to_string(),
            text: Some(format!("Model routing request: {text}\n\nUse tools/combo_list and tools/combo_create via MCP to manage model combos.")),
            metadata: None,
        },
    ]
}

fn handle_proxy_skill(text: &str) -> Vec<TaskPart> {
    vec![
        TaskPart {
            r#type: "text".to_string(),
            text: Some(format!("Proxy pool request: {text}\n\nUse tools/pool_list, tools/pool_create, or tools/pool_delete via MCP to manage proxy pools.")),
            metadata: None,
        },
    ]
}

fn handle_health_skill() -> Vec<TaskPart> {
    vec![TaskPart {
        r#type: "text".to_string(),
        text: Some(format!(
            "OpenProxy v{} is running.",
            env!("CARGO_PKG_VERSION")
        )),
        metadata: None,
    }]
}

fn handle_generic_task(text: &str, state: &crate::server::state::AppState) -> Vec<TaskPart> {
    let snap = state.db.snapshot();
    let info = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "providers": snap.provider_connections.len(),
        "combos": snap.combos.len(),
        "api_keys": snap.api_keys.len(),
        "proxy_pools": snap.proxy_pools.len(),
        "nodes": snap.provider_nodes.len(),
    });
    vec![TaskPart {
        r#type: "text".to_string(),
        text: Some(format!(
            "{text}\n\nServer state:\n{}",
            serde_json::to_string_pretty(&info).unwrap_or_default()
        )),
        metadata: None,
    }]
}
