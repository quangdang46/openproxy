//! Port of `open-sse/utils/kiroSessionReplay.js`.
//!
//! Preserve Kiro prompt-cacheability by freezing the first user message (`msg0`)
//! for a session, replaying that exact message as the first history user on later
//! turns, and injecting volatile current-time context only into the current turn.

use dashmap::DashMap;
use once_cell::sync::Lazy;
use serde_json::Value;
use std::time::Instant;

use crate::core::config::runtime_config::memory_config;

/// Hard cap on cached session-start entries (matches 9router MAX_SESSION_STARTS).
const MAX_SESSION_STARTS: usize = 5000;

#[derive(Debug, Clone)]
struct SessionStartEntry {
    session_start: Value,
    model_id: String,
    system_prompt: String,
    last_used: Instant,
}

static SESSION_START_STORE: Lazy<DashMap<String, SessionStartEntry>> = Lazy::new(DashMap::new);
static CLEANUP_LOCK: Lazy<parking_lot::Mutex<Instant>> =
    Lazy::new(|| parking_lot::Mutex::new(Instant::now()));

/// Result of applying Kiro session-replay rules to history + current message.
#[derive(Debug, Clone)]
pub struct KiroSessionReplayResult {
    pub history: Vec<Value>,
    pub current_message: Value,
    pub replayed: bool,
}

fn session_key(connection_id: &str, conversation_id: &str) -> String {
    format!("{connection_id}:{conversation_id}")
}

fn ensure_user_message_model_id(message: &mut Value, model_id: &str) {
    if model_id.is_empty() {
        return;
    }
    let Some(uim) = message.get_mut("userInputMessage") else {
        return;
    };
    let missing = uim
        .get("modelId")
        .and_then(|v| v.as_str())
        .map(|s| s.is_empty())
        .unwrap_or(true);
    if missing {
        if let Some(obj) = uim.as_object_mut() {
            obj.insert("modelId".into(), Value::String(model_id.to_string()));
        }
    }
}

fn ensure_history_model_ids(history: &mut [Value], model_id: &str) {
    for item in history.iter_mut() {
        ensure_user_message_model_id(item, model_id);
    }
}

fn prefix_user_message(message: &Value, content_prefix: &str, model_id: &str) -> Value {
    let mut out = message.clone();
    if out.get("userInputMessage").is_none() {
        out = serde_json::json!({ "userInputMessage": { "content": "" } });
    }
    ensure_user_message_model_id(&mut out, model_id);
    if !content_prefix.is_empty() {
        if let Some(uim) = out.get_mut("userInputMessage") {
            let content = uim
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let next = if content.is_empty() {
                content_prefix.to_string()
            } else {
                format!("{content_prefix}\n\n{content}")
            };
            if let Some(obj) = uim.as_object_mut() {
                obj.insert("content".into(), Value::String(next));
            }
        }
    }
    out
}

fn find_first_user_index(history: &[Value]) -> Option<usize> {
    history
        .iter()
        .position(|item| item.get("userInputMessage").is_some())
}

fn remember_session_start(key: String, entry: SessionStartEntry) {
    if SESSION_START_STORE.len() >= MAX_SESSION_STARTS {
        if let Some(victim) = SESSION_START_STORE.iter().next().map(|r| r.key().clone()) {
            SESSION_START_STORE.remove(&victim);
        }
    }
    SESSION_START_STORE.insert(key, entry);
}

fn maybe_run_cleanup() {
    let interval = memory_config::SESSION_CLEANUP_INTERVAL;
    let mut last = match CLEANUP_LOCK.try_lock() {
        Some(g) => g,
        None => return,
    };
    if last.elapsed() < interval {
        return;
    }
    let ttl = memory_config::SESSION_TTL;
    let cutoff = Instant::now() - ttl;
    SESSION_START_STORE.retain(|_, entry| entry.last_used >= cutoff);
    *last = Instant::now();
}

/// Apply Kiro multi-turn session-replay rules.
///
/// - First turn for a `(connection_id, conversation_id)` pair freezes `msg0`
///   (the first history user message, or the current message when history is empty).
/// - Later turns with the same model + system prompt replace the first history
///   user message with the frozen `msg0` and only stamp volatile time onto the
///   current turn via `current_content_prefix`.
pub fn apply_kiro_session_replay(
    conversation_id: Option<&str>,
    connection_id: Option<&str>,
    model_id: &str,
    system_prompt: &str,
    content_prefix: &str,
    current_content_prefix: &str,
    history: &[Value],
    current_message: &Value,
) -> KiroSessionReplayResult {
    maybe_run_cleanup();

    let connection_id = connection_id.unwrap_or("");
    let conversation_id = conversation_id.unwrap_or("");
    let key = session_key(connection_id, conversation_id);

    let existing = if !conversation_id.is_empty() {
        SESSION_START_STORE.get(&key).map(|e| e.clone())
    } else {
        None
    };

    let mut base_history = history.to_vec();
    let base_current = if current_message.is_null() {
        serde_json::json!({ "userInputMessage": { "content": "" } })
    } else {
        current_message.clone()
    };

    if let Some(existing) = existing {
        if existing.model_id == model_id && existing.system_prompt == system_prompt {
            if let Some(mut entry) = SESSION_START_STORE.get_mut(&key) {
                entry.last_used = Instant::now();
            }
            let mut session_start = existing.session_start.clone();
            ensure_user_message_model_id(&mut session_start, model_id);
            match find_first_user_index(&base_history) {
                Some(idx) => base_history[idx] = session_start,
                None => base_history.insert(0, session_start),
            }
            ensure_history_model_ids(&mut base_history, model_id);
            return KiroSessionReplayResult {
                history: base_history,
                current_message: prefix_user_message(
                    &base_current,
                    current_content_prefix,
                    model_id,
                ),
                replayed: true,
            };
        }
    }

    let first_user_index = find_first_user_index(&base_history);
    let (session_start, next_current) = if let Some(idx) = first_user_index {
        let session_start = prefix_user_message(&base_history[idx], content_prefix, model_id);
        base_history[idx] = session_start.clone();
        let next_current = prefix_user_message(&base_current, current_content_prefix, model_id);
        (session_start, next_current)
    } else {
        let session_start = prefix_user_message(&base_current, content_prefix, model_id);
        (session_start.clone(), session_start)
    };

    if !conversation_id.is_empty() {
        remember_session_start(
            key,
            SessionStartEntry {
                session_start: session_start.clone(),
                model_id: model_id.to_string(),
                system_prompt: system_prompt.to_string(),
                last_used: Instant::now(),
            },
        );
    }

    ensure_history_model_ids(&mut base_history, model_id);
    KiroSessionReplayResult {
        history: base_history,
        current_message: next_current,
        replayed: false,
    }
}

/// Drop all cached session-start entries. Useful for tests.
#[allow(dead_code)]
pub fn clear_kiro_session_replay_store() {
    SESSION_START_STORE.clear();
}

/// Number of cached entries. Useful for tests and observability.
#[allow(dead_code)]
pub fn cached_count() -> usize {
    SESSION_START_STORE.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn user_msg(content: &str, model: &str) -> Value {
        serde_json::json!({
            "userInputMessage": {
                "content": content,
                "modelId": model
            }
        })
    }

    #[test]
    fn freezes_msg0_across_turns_with_stable_conversation() {
        let _guard = TEST_LOCK.lock().unwrap();
        clear_kiro_session_replay_store();

        let model = "claude-sonnet-4.5";
        let first_current = user_msg("first turn", model);
        let first = apply_kiro_session_replay(
            Some("conv-1"),
            Some("conn-1"),
            model,
            "",
            "[Context: Current time is T1]",
            "[Context: Current time is T1]",
            &[],
            &first_current,
        );
        assert!(!first.replayed);
        assert!(first.current_message["userInputMessage"]["content"]
            .as_str()
            .unwrap()
            .contains("first turn"));
        assert!(first.current_message["userInputMessage"]["content"]
            .as_str()
            .unwrap()
            .contains("Current time is T1"));

        // Second turn: history has the frozen first message as prior user turn.
        let history = vec![first.current_message.clone()];
        let second_current = user_msg("second turn", model);
        let second = apply_kiro_session_replay(
            Some("conv-1"),
            Some("conn-1"),
            model,
            "",
            "[Context: Current time is T1]",
            "[Context: Current time is T2]",
            &history,
            &second_current,
        );
        assert!(second.replayed);
        assert_eq!(
            second.history[0]["userInputMessage"]["content"],
            first.current_message["userInputMessage"]["content"]
        );
        let cur = second.current_message["userInputMessage"]["content"]
            .as_str()
            .unwrap();
        assert!(cur.contains("Current time is T2"));
        assert!(cur.contains("second turn"));
        // Volatile time for turn 1 must not leak into current turn content.
        assert!(!cur.contains("Current time is T1"));
    }

    #[test]
    fn skips_store_when_conversation_id_missing() {
        let _guard = TEST_LOCK.lock().unwrap();
        clear_kiro_session_replay_store();
        let model = "m";
        let _ = apply_kiro_session_replay(
            None,
            Some("conn"),
            model,
            "",
            "prefix",
            "cur",
            &[],
            &user_msg("hi", model),
        );
        assert_eq!(cached_count(), 0);
    }
}
