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
/// `provider` scopes the rule: `None` matches every provider, `Some(id)`
/// only fires when the resolved provider id equals it (9router
/// errorConfig.js:61-64 — e.g. the codex "model is not supported" and
/// codebuddy-intl "request illegal" rules must not cool down unrelated
/// providers that happen to emit the same text).
#[derive(Debug, Clone, Copy)]
pub struct ErrorRule {
    pub provider: Option<&'static str>,
    pub text: Option<&'static str>,
    pub status: Option<u16>,
    pub cooldown: Option<Duration>,
    pub backoff: bool,
}

/// Unified error classification table, checked top-to-bottom. Provider-
/// scoped text rules fire before unscoped text rules; status rules are the
/// fallback. Order = priority (9router errorConfig.js:59-82).
pub const ERROR_RULES: &[ErrorRule] = &[
    // --- Provider-scoped text rules (checked first, order = priority) ---
    // Codex chatgpt-account model gating: park for the full 30-minute cap.
    ErrorRule {
        provider: Some("codex"),
        text: Some("model is not supported when using codex with a chatgpt account"),
        status: None,
        cooldown: Some(Duration::from_millis(MAX_RATE_LIMIT_COOLDOWN_MS)),
        backoff: false,
    },
    // Intl gateway blocks affect the account/IP, so let the connection back off.
    ErrorRule {
        provider: Some("codebuddy-intl"),
        text: Some("request illegal"),
        status: None,
        cooldown: None,
        backoff: true,
    },
    ErrorRule {
        provider: Some("codebuddy-intl"),
        text: Some("conversation id missing"),
        status: None,
        cooldown: None,
        backoff: true,
    },
    // --- Text-based rules (case-insensitive substring match). ---
    ErrorRule {
        provider: None,
        text: Some("no credentials"),
        status: None,
        cooldown: Some(Duration::from_millis(cooldown_consts::LONG_MS)),
        backoff: false,
    },
    ErrorRule {
        provider: None,
        text: Some("request not allowed"),
        status: None,
        cooldown: Some(Duration::from_millis(cooldown_consts::SHORT_MS)),
        backoff: false,
    },
    ErrorRule {
        provider: None,
        text: Some("improperly formed request"),
        status: None,
        cooldown: Some(Duration::from_millis(cooldown_consts::LONG_MS)),
        backoff: false,
    },
    ErrorRule {
        provider: None,
        text: Some("rate limit"),
        status: None,
        cooldown: None,
        backoff: true,
    },
    ErrorRule {
        provider: None,
        text: Some("too many requests"),
        status: None,
        cooldown: None,
        backoff: true,
    },
    ErrorRule {
        provider: None,
        text: Some("quota exceeded"),
        status: None,
        cooldown: None,
        backoff: true,
    },
    ErrorRule {
        provider: None,
        text: Some("insufficient credit"),
        status: None,
        cooldown: None,
        backoff: true,
    },
    ErrorRule {
        provider: None,
        text: Some("credit balance is too low"),
        status: None,
        cooldown: None,
        backoff: true,
    },
    ErrorRule {
        provider: None,
        text: Some("capacity"),
        status: None,
        cooldown: None,
        backoff: true,
    },
    ErrorRule {
        provider: None,
        text: Some("overloaded"),
        status: None,
        cooldown: None,
        backoff: true,
    },
    // Status-based fallbacks.
    ErrorRule {
        provider: None,
        text: None,
        status: Some(401),
        cooldown: Some(Duration::from_millis(cooldown_consts::LONG_MS)),
        backoff: false,
    },
    ErrorRule {
        provider: None,
        text: None,
        status: Some(402),
        cooldown: Some(Duration::from_millis(cooldown_consts::LONG_MS)),
        backoff: false,
    },
    ErrorRule {
        provider: None,
        text: None,
        status: Some(403),
        cooldown: Some(Duration::from_millis(cooldown_consts::LONG_MS)),
        backoff: false,
    },
    ErrorRule {
        provider: None,
        text: None,
        status: Some(404),
        cooldown: Some(Duration::from_millis(cooldown_consts::LONG_MS)),
        backoff: false,
    },
    ErrorRule {
        provider: None,
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
    /// Request-scoped client error: the REQUEST itself is what the member
    /// rejected (context overflow, malformed body, unsupported parameter) —
    /// the credential is healthy, so no cooldown applies and the combo
    /// rotates to the next member instead
    /// (9router accountFallback.js:48-61 `requestScoped`).
    RequestScoped,
}

/// Run an upstream error through [`ERROR_RULES`] and return the matching
/// classification. Provider-scoped rules only fire when `provider` (an
/// alias or canonical id — resolved via
/// [`crate::core::model::resolve_provider_alias`], matching 9router
/// `resolveProviderId`) equals the rule's provider. Text rules fire first;
/// status rules are the fallback.
///
/// An error that matches nothing — a bare 400, a 418, a 500 — is
/// [`NoMatch`], and the caller gives it the 30 s transient cooldown
/// (`TRANSIENT_COOLDOWN_MS`, errorConfig.js:39). 9router has no "permanent"
/// tier: `checkFallbackError` always returns `shouldFallback: true` and picks
/// a duration purely from this table (accountFallback.js:23-50), so a 400 is
/// worth exactly as much as a 500.
pub fn classify_error(
    message: Option<&str>,
    status: Option<u16>,
    provider: Option<&str>,
) -> ErrorClassification {
    let lowered = message.map(|m| m.to_lowercase());
    let resolved_provider;
    let provider_id = match provider {
        Some(p) => {
            resolved_provider = crate::core::model::resolve_provider_alias(p);
            Some(resolved_provider.as_str())
        }
        None => None,
    };
    for rule in ERROR_RULES {
        // 9router accountFallback.js:30 — `if (rule.provider && rule.provider
        // !== provider) continue;`: scoped rules are invisible to other
        // providers, and invisible when no provider is known.
        if let Some(want) = rule.provider {
            if provider_id != Some(want) {
                continue;
            }
        }
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

    // 9router accountFallback.js:48-61 — request-scoped client errors that
    // matched no rule above: a 4xx caused by the request itself (context
    // overflow, malformed body, unsupported parameter) says nothing about
    // the credential, so cooling the account down only removes a healthy
    // connection from rotation. Account-scoped statuses keep their rules
    // above (401/402/403/404/429); 404 can never reach this arm because its
    // status rule always matches first, exactly like upstream.
    if let Some(s) = status {
        if (400..500).contains(&s) && ![401u16, 402, 403, 429].contains(&s) {
            return ErrorClassification::RequestScoped;
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
            classify_error(Some("Rate limit exceeded"), Some(500), None),
            ErrorClassification::Backoff
        );
    }

    #[test]
    fn classify_falls_back_to_status() {
        assert_eq!(
            classify_error(Some("auth failed"), Some(401), None),
            ErrorClassification::Cooldown(Duration::from_millis(COOLDOWN_LONG_MS))
        );
    }

    #[test]
    fn classify_no_match_returns_unmatched() {
        assert_eq!(
            classify_error(Some("teapot"), Some(418), None),
            ErrorClassification::RequestScoped
        );
        // 5xx and sub-400 statuses still fall through to NoMatch.
        assert_eq!(
            classify_error(Some("teapot"), Some(500), None),
            ErrorClassification::NoMatch
        );
    }

    // 9router errorConfig.js:59-76 declares 401/402/403/404 as long cooldowns
    // and 429 as backoff. 400 is deliberately absent — a malformed request is
    // the single most common recoverable 4xx on these upstreams (a bad tool
    // schema, an over-long field), and it clears on the next well-formed
    // request, so it belongs on the 30 s transient default.
    #[test]
    fn the_status_table_matches_the_js_rule_list() {
        for status in [401u16, 402, 403, 404] {
            assert_eq!(
                classify_error(Some("boom"), Some(status), None),
                ErrorClassification::Cooldown(Duration::from_millis(COOLDOWN_LONG_MS)),
                "status {status} takes the long cooldown"
            );
        }
        assert_eq!(
            classify_error(Some("boom"), Some(429), None),
            ErrorClassification::Backoff
        );
    }

    #[test]
    fn the_402_status_rule_is_present() {
        assert!(
            ERROR_RULES.iter().any(|r| r.status == Some(402)
                && !r.backoff
                && r.cooldown == Some(Duration::from_millis(COOLDOWN_LONG_MS))),
            "9router errorConfig.js:72 has a 402 status rule; without it a \
             payment-required response has no fixed cooldown"
        );
    }

    #[test]
    fn an_unmatched_400_is_request_scoped_not_transient() {
        // 9router accountFallback.js:48-61: a 4xx no rule claimed is caused by
        // the REQUEST (context overflow, malformed body, unsupported
        // parameter) — the credential is healthy, so there is no cooldown at
        // all. It is resolved into RequestScoped, not the transient default.
        assert_eq!(
            classify_error(Some("unsupported parameter: max_tokens"), Some(400), None),
            ErrorClassification::RequestScoped
        );
        // Account-scoped statuses keep their rules: 401/402/403/404/429 never
        // reach the request-scoped arm.
        for status in [401u16, 402, 403, 404, 429] {
            assert_ne!(
                classify_error(Some("boom"), Some(status), None),
                ErrorClassification::RequestScoped,
                "status {status} is account-scoped"
            );
        }
        // A text rule still wins over the request-scoped arm — "rate limit"
        // wording on a 400 backs off exactly like upstream.
        assert_eq!(
            classify_error(Some("rate limit exceeded"), Some(400), None),
            ErrorClassification::Backoff
        );
    }

    // --- 9router v0.5.112 errorConfig.js:61-74 additions ---

    #[test]
    fn codex_unsupported_model_takes_the_30min_cap() {
        // errorConfig.js:61 — scoped to codex, fixed MAX_RATE_LIMIT_COOLDOWN_MS.
        assert_eq!(
            classify_error(
                Some("The model is not supported when using Codex with a ChatGPT account"),
                Some(400),
                Some("codex"),
            ),
            ErrorClassification::Cooldown(Duration::from_millis(MAX_RATE_LIMIT_COOLDOWN_MS))
        );
        // The same text from another provider must NOT hit the scoped rule —
        // it falls through to the request-scoped 4xx arm (accountFallback.js:48-61).
        assert_eq!(
            classify_error(
                Some("model is not supported when using codex with a chatgpt account"),
                Some(400),
                Some("openai"),
            ),
            ErrorClassification::RequestScoped
        );
        // And with no provider at all the scoped rule is invisible — same
        // request-scoped outcome.
        assert_eq!(
            classify_error(
                Some("model is not supported when using codex with a chatgpt account"),
                Some(400),
                None,
            ),
            ErrorClassification::RequestScoped
        );
    }

    #[test]
    fn codebuddy_intl_gateway_blocks_back_off() {
        // errorConfig.js:63-64 — provider-scoped backoff rules.
        for text in ["request illegal", "conversation id missing"] {
            assert_eq!(
                classify_error(Some(text), Some(400), Some("codebuddy-intl")),
                ErrorClassification::Backoff,
                "{text} on codebuddy-intl backs off"
            );
            // Scoped: the same wording on codebuddy-cn does not fire it.
            assert_ne!(
                classify_error(Some(text), Some(400), Some("codebuddy-cn")),
                ErrorClassification::Backoff,
                "{text} is codebuddy-intl only"
            );
        }
    }

    #[test]
    fn insufficient_credit_rules_back_off() {
        // errorConfig.js:71-72 — unscoped backoff rules.
        for text in ["insufficient credit", "credit balance is too low"] {
            assert_eq!(
                classify_error(Some(text), Some(402), Some("openrouter")),
                ErrorClassification::Backoff,
                "{text} backs off on any provider"
            );
        }
    }

    #[test]
    fn provider_aliases_resolve_for_scoped_rules() {
        // cbai is the codebuddy-intl alias (model/mod.rs:151) — a scoped rule
        // must still fire when the caller passes the alias, matching 9router
        // resolveProviderId.
        assert_eq!(
            classify_error(Some("request illegal"), Some(400), Some("cbai")),
            ErrorClassification::Backoff
        );
    }
}
