//! Request logging for observability.
//!
//! Tracks all API requests with timing, status, and token usage.

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A single request log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLog {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub method: String,
    pub path: String,
    pub model: Option<String>,
    pub request_tokens: Option<u32>,
    pub response_tokens: Option<u32>,
    pub status_code: u16,
    pub duration_ms: u64,
    pub error: Option<String>,
}

/// Shared request logger for storing request records.
pub struct RequestLogger {
    logs: Mutex<Vec<RequestLog>>,
    max_records: usize,
}

impl RequestLogger {
    pub fn new(max_records: usize) -> Self {
        Self {
            logs: Mutex::new(Vec::new()),
            max_records: max_records.max(1),
        }
    }

    /// Log a request with all details.
    pub fn log(&self, entry: RequestLog) {
        let mut logs = self.logs.lock().unwrap();
        logs.push(entry);
        if logs.len() > self.max_records {
            logs.remove(0);
        }
    }

    /// Get all logs, newest first.
    pub fn get_logs(&self) -> Vec<RequestLog> {
        let logs = self.logs.lock().unwrap();
        let mut result = logs.clone();
        result.reverse();
        result
    }

    /// Clear all logs.
    pub fn clear(&self) {
        let mut logs = self.logs.lock().unwrap();
        logs.clear();
    }

    /// Generate aggregate statistics.
    pub fn get_stats(&self) -> ObservabilityStats {
        let logs = self.logs.lock().unwrap();
        
        let total_requests = logs.len() as u64;
        let mut total_request_tokens = 0u64;
        let mut total_response_tokens = 0u64;
        let mut total_duration_ms = 0u64;
        let mut success_count = 0u64;
        let mut error_count = 0u64;
        let mut status_codes: HashMap<u16, u64> = HashMap::new();
        let mut models: HashMap<String, u64> = HashMap::new();

        for log in logs.iter() {
            total_request_tokens += log.request_tokens.unwrap_or(0) as u64;
            total_response_tokens += log.response_tokens.unwrap_or(0) as u64;
            total_duration_ms += log.duration_ms;

            if log.status_code < 400 {
                success_count += 1;
            } else {
                error_count += 1;
            }

            *status_codes.entry(log.status_code).or_insert(0) += 1;

            if let Some(model) = &log.model {
                *models.entry(model.clone()).or_insert(0) += 1;
            }
        }

        let avg_duration_ms = if total_requests > 0 {
            total_duration_ms / total_requests
        } else {
            0
        };

        ObservabilityStats {
            total_requests,
            total_request_tokens,
            total_response_tokens,
            total_duration_ms,
            avg_duration_ms,
            success_count,
            error_count,
            status_codes,
            top_models: models,
        }
    }
}

/// Aggregate statistics for observability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservabilityStats {
    pub total_requests: u64,
    pub total_request_tokens: u64,
    pub total_response_tokens: u64,
    pub total_duration_ms: u64,
    pub avg_duration_ms: u64,
    pub success_count: u64,
    pub error_count: u64,
    pub status_codes: HashMap<u16, u64>,
    pub top_models: HashMap<String, u64>,
}

/// Create a new RequestLog with a generated ID.
pub fn create_request_log(
    method: &str,
    path: &str,
    model: Option<String>,
    request_tokens: Option<u32>,
    response_tokens: Option<u32>,
    status_code: u16,
    duration_ms: u64,
    error: Option<String>,
) -> RequestLog {
    RequestLog {
        id: Uuid::new_v4().to_string(),
        timestamp: Utc::now(),
        method: method.to_string(),
        path: path.to_string(),
        model,
        request_tokens,
        response_tokens,
        status_code,
        duration_ms,
        error,
    }
}
