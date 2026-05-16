//! Port of `open-sse/config/errorConfig.js` — error type/message tables,
//! backoff configuration, and the unified rule list used to decide cooldown
//! durations from upstream errors.

use std::time::Duration;

/// Cooldown duration constants (ms-equivalent) used by [`ERROR_RULES`] and
/// the `COOLDOWN_MS` backwards-compat record.
mod cooldown_consts {
    pub const LONG_MS: u64 = 2 * 60 * 1000;
    pub const SHORT_MS: u64 = 5 * 1000;
    pub const TRANSIENT_MS: u64 = 30 * 1000;
}

/// Long cooldown for credential / auth-related errors that need user action.
pub const COOLDOWN_LONG_MS: u64 = cooldown_consts::LONG_MS;
/// Short cooldown for "request not allowed" style soft-rejection errors.
pub const COOLDOWN_SHORT_MS: u64 = cooldown_consts::SHORT_MS;
/// Default cooldown for transient/unknown errors.
pub const TRANSIENT_COOLDOWN_MS: u64 = cooldown_consts::TRANSIENT_MS;

/// Hard cap for provider-reported rate-limit cooldowns (some providers
/// like Codex announce `resets_at` 5–6 hours out, which we never honour
/// directly — clamp to 30 minutes).
pub const MAX_RATE_LIMIT_COOLDOWN_MS: u64 = 30 * 60 * 1000;

/// Exponential backoff parameters for rate-limit retries.
#[derive(Debug, Clone, Copy)]
pub struct BackoffConfig {
    pub base_ms: u64,
    pub max_ms: u64,
    pub max_level: u32,
}

pub const BACKOFF_CONFIG: BackoffConfig = BackoffConfig {
    base_ms: 2000,
    max_ms: 5 * 60 * 1000,
    max_level: 15,
};

/// OpenAI-compatible error type/code descriptor surfaced to clients.
#[derive(Debug, Clone, Copy)]
pub struct ErrorTypeInfo {
    pub r#type: &'static str,
    pub code: &'static str,
}

/// Translate an HTTP status code to an OpenAI-compatible error type/code.
pub const fn error_type_for(status: u16) -> Option<ErrorTypeInfo> {
    Some(match status {
        400 => ErrorTypeInfo {
            r#type: "invalid_request_error",
            code: "bad_request",
        },
        401 => ErrorTypeInfo {
            r#type: "authentication_error",
            code: "invalid_api_key",
        },
        402 => ErrorTypeInfo {
            r#type: "billing_error",
            code: "payment_required",
        },
        403 => ErrorTypeInfo {
            r#type: "permission_error",
            code: "insufficient_quota",
        },
        404 => ErrorTypeInfo {
            r#type: "invalid_request_error",
            code: "model_not_found",
        },
        406 => ErrorTypeInfo {
            r#type: "invalid_request_error",
            code: "model_not_supported",
        },
        429 => ErrorTypeInfo {
            r#type: "rate_limit_error",
            code: "rate_limit_exceeded",
        },
        500 => ErrorTypeInfo {
            r#type: "server_error",
            code: "internal_server_error",
        },
        502 => ErrorTypeInfo {
            r#type: "server_error",
            code: "bad_gateway",
        },
        503 => ErrorTypeInfo {
            r#type: "server_error",
            code: "service_unavailable",
        },
        504 => ErrorTypeInfo {
            r#type: "server_error",
            code: "gateway_timeout",
        },
        _ => return None,
    })
}

/// Default client-facing error message for each known status code.
pub const fn default_error_message(status: u16) -> Option<&'static str> {
    Some(match status {
        400 => "Bad request",
        401 => "Invalid API key provided",
        402 => "Payment required",
        403 => "You exceeded your current quota",
        404 => "Model not found",
        406 => "Model not supported",
        429 => "Rate limit exceeded",
        500 => "Internal server error",
        502 => "Bad gateway - upstream provider error",
        503 => "Service temporarily unavailable",
        504 => "Gateway timeout",
        _ => return None,
    })
}

/// One classification rule. Either matches by substring on the error
/// message (`text`) or by HTTP status (`status`). When matched, `cooldown`
/// gives the suggested cooldown duration; if `backoff` is true the rate-
/// limit exponential backoff is used instead.
#[derive(Debug, Clone, Copy)]
pub struct ErrorRule {
    pub text: Option<&'static str>,
    pub status: Option<u16>,
    pub cooldown: Option<Duration>,
    pub backoff: bool,
}

/// Unified error classification table, checked top-to-bottom. Text rules
/// fire before status rules.
pub const ERROR_RULES: &[ErrorRule] = &[
    // Text-based rules (case-insensitive substring match).
    ErrorRule {
        text: Some("no credentials"),
        status: None,
        cooldown: Some(Duration::from_millis(cooldown_consts::LONG_MS)),
        backoff: false,
    },
    ErrorRule {
        text: Some("request not allowed"),
        status: None,
        cooldown: Some(Duration::from_millis(cooldown_consts::SHORT_MS)),
        backoff: false,
    },
    ErrorRule {
        text: Some("improperly formed request"),
        status: None,
        cooldown: Some(Duration::from_millis(cooldown_consts::LONG_MS)),
        backoff: false,
    },
    ErrorRule {
        text: Some("rate limit"),
        status: None,
        cooldown: None,
        backoff: true,
    },
    ErrorRule {
        text: Some("too many requests"),
        status: None,
        cooldown: None,
        backoff: true,
    },
    ErrorRule {
        text: Some("quota exceeded"),
        status: None,
        cooldown: None,
        backoff: true,
    },
    ErrorRule {
        text: Some("capacity"),
        status: None,
        cooldown: None,
        backoff: true,
    },
    ErrorRule {
        text: Some("overloaded"),
        status: None,
        cooldown: None,
        backoff: true,
    },
    // Status-based fallbacks.
    ErrorRule {
        text: None,
        status: Some(401),
        cooldown: Some(Duration::from_millis(cooldown_consts::LONG_MS)),
        backoff: false,
    },
    ErrorRule {
        text: None,
        status: Some(402),
        cooldown: Some(Duration::from_millis(cooldown_consts::LONG_MS)),
        backoff: false,
    },
    ErrorRule {
        text: None,
        status: Some(403),
        cooldown: Some(Duration::from_millis(cooldown_consts::LONG_MS)),
        backoff: false,
    },
    ErrorRule {
        text: None,
        status: Some(404),
        cooldown: Some(Duration::from_millis(cooldown_consts::LONG_MS)),
        backoff: false,
    },
    ErrorRule {
        text: None,
        status: Some(429),
        cooldown: None,
        backoff: true,
    },
];

/// Outcome of classifying a single upstream error against [`ERROR_RULES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClassification {
    /// Use exponential backoff (rate-limit path).
    Backoff,
    /// Apply the given fixed cooldown duration.
    Cooldown(Duration),
    /// No rule matched; caller should apply their own default.
    NoMatch,
}

/// Run an upstream error through [`ERROR_RULES`] and return the matching
/// classification. Text rules fire first; status rules are the fallback.
pub fn classify_error(message: Option<&str>, status: Option<u16>) -> ErrorClassification {
    let lowered = message.map(|m| m.to_lowercase());
    for rule in ERROR_RULES {
        let matched = match (rule.text, rule.status) {
            (Some(needle), _) => lowered
                .as_deref()
                .map(|m| m.contains(needle))
                .unwrap_or(false),
            (None, Some(want)) => status == Some(want),
            (None, None) => false,
        };
        if !matched {
            continue;
        }
        if rule.backoff {
            return ErrorClassification::Backoff;
        }
        if let Some(d) = rule.cooldown {
            return ErrorClassification::Cooldown(d);
        }
    }
    ErrorClassification::NoMatch
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_type_round_trip() {
        let info = error_type_for(429).unwrap();
        assert_eq!(info.r#type, "rate_limit_error");
        assert_eq!(info.code, "rate_limit_exceeded");
        assert!(error_type_for(999).is_none());
    }

    #[test]
    fn classify_picks_text_rule_first() {
        // Status 500 wouldn't match any rule, but the text "rate limit" wins.
        assert_eq!(
            classify_error(Some("Rate limit exceeded"), Some(500)),
            ErrorClassification::Backoff
        );
    }

    #[test]
    fn classify_falls_back_to_status() {
        assert_eq!(
            classify_error(Some("auth failed"), Some(401)),
            ErrorClassification::Cooldown(Duration::from_millis(COOLDOWN_LONG_MS))
        );
    }

    #[test]
    fn classify_no_match_returns_unmatched() {
        assert_eq!(
            classify_error(Some("teapot"), Some(418)),
            ErrorClassification::NoMatch
        );
    }
}
