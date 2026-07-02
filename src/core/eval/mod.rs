/// LLM evaluation framework for OpenProxy.
///
/// This module provides the types and machinery to define, run, and store
/// golden test evaluations against LLM providers. Results are used to inform
/// routing decisions – e.g., preferring a provider that passes a reliability
/// eval over one that doesn't.
///
/// # Quick start
///
/// ```ignore
/// use crate::core::eval::{EvalSuite, EvalCase, EvalResult, EvalScore};
///
/// let suite = EvalSuite::new("translation-fr")
///     .with_description("French translation accuracy")
///     .add_case(EvalCase::new("hello-world", "Translate to French", "Bonjour le monde")
///         .with_expected_substring("Bonjour"));
///
/// let result = suite.run("claude", &response_text).unwrap();
/// ```
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// Identifier for an eval case, intended to be human-readable.
pub type CaseId = String;

/// A single eval case: a golden test that checks whether the provider's output
/// satisfies some criteria.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalCase {
    /// Unique identifier for this case.
    pub id: CaseId,
    /// Human-readable description of what this case tests.
    pub description: String,
    /// The expected output or ground-truth answer.
    pub expected: String,
    /// Optional substring that must appear in the provider output.
    pub expected_substring: Option<String>,
    /// Optional regex that must match in the provider output.
    pub expected_pattern: Option<String>,
    /// Expected maximum output length (characters).
    pub max_length: Option<usize>,
    /// Expected minimum output length (characters).
    pub min_length: Option<usize>,
    /// Tags for grouping / filtering.
    pub tags: Vec<String>,
    /// Arbitrary metadata.
    pub metadata: BTreeMap<String, String>,
}

impl EvalCase {
    /// Create a new eval case with the given id, description, and expected value.
    pub fn new(
        id: impl Into<String>,
        description: impl Into<String>,
        expected: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            expected: expected.into(),
            expected_substring: None,
            expected_pattern: None,
            max_length: None,
            min_length: None,
            tags: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    /// Builder: set the description (overrides the one from [`new`]).
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Builder: set an expected substring.
    pub fn with_expected_substring(mut self, s: impl Into<String>) -> Self {
        self.expected_substring = Some(s.into());
        self
    }

    /// Builder: set an expected regex pattern.
    pub fn with_expected_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.expected_pattern = Some(pattern.into());
        self
    }

    /// Builder: set maximum output length.
    pub fn with_max_length(mut self, n: usize) -> Self {
        self.max_length = Some(n);
        self
    }

    /// Builder: set minimum output length.
    pub fn with_min_length(mut self, n: usize) -> Self {
        self.min_length = Some(n);
        self
    }

    /// Builder: add a tag.
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Builder: add metadata.
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Evaluate this case against the given provider output.
    ///
    /// Returns `true` if all checks pass.
    pub fn evaluate(&self, output: &str) -> EvalScore {
        let mut passed = true;
        let mut details: Vec<String> = Vec::new();

        // Substring check
        if let Some(sub) = &self.expected_substring {
            if output.contains(sub.as_str()) {
                details.push(format!("substring '{}' found", sub));
            } else {
                passed = false;
                details.push(format!("substring '{}' NOT found", sub));
            }
        }

        // Regex check
        if let Some(pattern) = &self.expected_pattern {
            match regex::Regex::new(pattern) {
                Ok(re) => {
                    if re.is_match(output) {
                        details.push(format!("pattern '{}' matched", pattern));
                    } else {
                        passed = false;
                        details.push(format!("pattern '{}' NOT matched", pattern));
                    }
                }
                Err(e) => {
                    passed = false;
                    details.push(format!("invalid regex '{}': {}", pattern, e));
                }
            }
        }

        // Length checks
        if let Some(max) = self.max_length {
            if output.len() <= max {
                details.push(format!("length {} <= {}", output.len(), max));
            } else {
                passed = false;
                details.push(format!("length {} exceeds max {}", output.len(), max));
            }
        }

        if let Some(min) = self.min_length {
            if output.len() >= min {
                details.push(format!("length {} >= {}", output.len(), min));
            } else {
                passed = false;
                details.push(format!("length {} below min {}", output.len(), min));
            }
        }

        EvalScore {
            case_id: self.id.clone(),
            passed,
            details,
        }
    }
}

/// Score for a single eval case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalScore {
    pub case_id: CaseId,
    pub passed: bool,
    pub details: Vec<String>,
}

/// Result of running a full eval suite against a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalResult {
    /// Suite name.
    pub suite: String,
    /// Provider name.
    pub provider: String,
    /// When the eval was run.
    pub timestamp: String,
    /// Duration of the evaluation.
    pub duration_ms: u64,
    /// Per-case scores.
    pub scores: Vec<EvalScore>,
    /// Aggregate pass rate (0.0 – 1.0).
    pub pass_rate: f64,
    /// Number of cases that passed.
    pub passed: usize,
    /// Number of cases that failed.
    pub failed: usize,
    /// Total number of cases.
    pub total: usize,
}

/// A suite of eval cases that can be run against a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalSuite {
    /// Name of this suite.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Cases in this suite.
    pub cases: Vec<EvalCase>,
    /// When this suite was last run (ISO 8601).
    pub last_run: Option<String>,
}

impl EvalSuite {
    /// Create a new eval suite with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            cases: Vec::new(),
            last_run: None,
        }
    }

    /// Builder: set description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Builder: add an eval case.
    pub fn add_case(mut self, case: EvalCase) -> Self {
        self.cases.push(case);
        self
    }

    /// Add an eval case (mutable reference variant).
    pub fn push_case(&mut self, case: EvalCase) {
        self.cases.push(case);
    }

    /// Run all cases in this suite against a single provider output string.
    ///
    /// This is a convenience that evaluates all cases against the same output.
    /// For real usage, each case would be a separate API call; see
    /// [`EvalRunner`] for structured execution.
    pub fn evaluate_all(&self, provider: &str, output: &str) -> EvalResult {
        let start = Instant::now();
        let mut scores = Vec::with_capacity(self.cases.len());

        for case in &self.cases {
            scores.push(case.evaluate(output));
        }

        let duration = start.elapsed();
        let passed = scores.iter().filter(|s| s.passed).count();
        let total = scores.len();

        EvalResult {
            suite: self.name.clone(),
            provider: provider.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            duration_ms: duration.as_millis() as u64,
            scores,
            pass_rate: if total > 0 {
                passed as f64 / total as f64
            } else {
                0.0
            },
            passed,
            failed: total - passed,
            total,
        }
    }

    /// Create a builder for a runner that will actually call the provider API.
    pub fn runner(&self) -> EvalRunnerBuilder {
        EvalRunnerBuilder::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// EvalRunner – actually calls providers
// ---------------------------------------------------------------------------

/// Builder for an [`EvalRunner`] that executes eval cases by sending requests
/// to a provider and evaluating the responses.
pub struct EvalRunnerBuilder {
    suite: EvalSuite,
    /// Timeout per individual request.
    request_timeout: Duration,
}

impl EvalRunnerBuilder {
    pub fn new(suite: EvalSuite) -> Self {
        Self {
            suite,
            request_timeout: Duration::from_secs(60),
        }
    }

    /// Set the per-request timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Build the runner.
    ///
    /// The runner requires a closure that can send a prompt to a provider and
    /// return the response text. This allows integration with whatever HTTP
    /// client or executor the calling code uses.
    pub fn build<F>(self, requester: F) -> EvalRunner<F>
    where
        F: Fn(&str, &str) -> Result<String, String>,
    {
        EvalRunner {
            suite: self.suite,
            requester,
            request_timeout: self.request_timeout,
        }
    }
}

/// An eval runner that drives actual API calls to providers.
pub struct EvalRunner<F> {
    suite: EvalSuite,
    requester: F,
    request_timeout: Duration,
}

impl<F> EvalRunner<F>
where
    F: Fn(&str, &str) -> Result<String, String>,
{
    /// Run the full suite against the given provider.
    ///
    /// `prompt_fn` maps each case to the actual prompt string to send (by
    /// default the case `expected` field is used as the prompt, but you may
    /// want a different mapping).
    pub fn run(&self, provider: &str, prompt_fn: impl Fn(&EvalCase) -> String) -> EvalResult {
        let start = Instant::now();
        let mut scores = Vec::with_capacity(self.suite.cases.len());

        for case in &self.suite.cases {
            let prompt = prompt_fn(case);
            match (self.requester)(provider, &prompt) {
                Ok(output) => {
                    scores.push(case.evaluate(&output));
                }
                Err(e) => {
                    scores.push(EvalScore {
                        case_id: case.id.clone(),
                        passed: false,
                        details: vec![format!("request failed: {}", e)],
                    });
                }
            }
        }

        let duration = start.elapsed();
        let passed = scores.iter().filter(|s| s.passed).count();
        let total = scores.len();

        EvalResult {
            suite: self.suite.name.clone(),
            provider: provider.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            duration_ms: duration.as_millis() as u64,
            scores,
            pass_rate: if total > 0 {
                passed as f64 / total as f64
            } else {
                0.0
            },
            passed,
            failed: total - passed,
            total,
        }
    }

    /// Get a reference to the suite.
    pub fn suite(&self) -> &EvalSuite {
        &self.suite
    }
}

// ---------------------------------------------------------------------------
// EvalResultStore – persist results
// ---------------------------------------------------------------------------

/// A simple in-memory store for eval results, keyed by provider name.
///
/// This is used by routing logic to factor in eval pass rates when choosing
/// which provider to use for a request.
#[derive(Debug, Clone)]
pub struct EvalResultStore {
    /// Map from provider name to its most recent eval result per suite.
    results: BTreeMap<String, BTreeMap<String, EvalResult>>,
}

impl EvalResultStore {
    /// Create a new empty store.
    pub fn new() -> Self {
        Self {
            results: BTreeMap::new(),
        }
    }

    /// Store an eval result for a provider.
    pub fn store(&mut self, provider: &str, result: EvalResult) {
        let suite = result.suite.clone();
        self.results
            .entry(provider.to_string())
            .or_default()
            .insert(suite, result);
    }

    /// Get the most recent eval result for a provider and suite.
    pub fn get(&self, provider: &str, suite: &str) -> Option<&EvalResult> {
        self.results.get(provider)?.get(suite)
    }

    /// Get all eval results for a provider.
    pub fn get_all(&self, provider: &str) -> Option<&BTreeMap<String, EvalResult>> {
        self.results.get(provider)
    }

    /// Get the pass rate for a provider on a specific suite.
    ///
    /// Returns `None` if no result is stored for that provider+suite.
    pub fn pass_rate(&self, provider: &str, suite: &str) -> Option<f64> {
        self.get(provider, suite).map(|r| r.pass_rate)
    }

    /// Remove all results for a provider.
    pub fn clear_provider(&mut self, provider: &str) {
        self.results.remove(provider);
    }

    /// Number of providers with stored results.
    pub fn len(&self) -> usize {
        self.results.len()
    }

    /// Check if the store is empty.
    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
    }
}

impl Default for EvalResultStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Built-in eval suites
// ---------------------------------------------------------------------------

/// Pre-built eval suites for common scenarios.
pub mod builtin {
    use super::*;

    /// A simple connectivity / basic-response test.
    pub fn connectivity_suite() -> EvalSuite {
        EvalSuite::new("connectivity")
            .with_description("Basic connectivity and response test")
            .add_case(
                EvalCase::new("echo", "Simple echo test", "Hello")
                    .with_description("Provider should respond to a simple greeting")
                    .with_tag("smoke"),
            )
    }

    /// A JSON output conformance suite.
    pub fn json_output_suite() -> EvalSuite {
        EvalSuite::new("json-output")
            .with_description("Provider should return valid JSON")
            .add_case(
                EvalCase::new("json-object", "Return a JSON object", r#"{"name":"test"}"#)
                    .with_expected_substring(r#""name""#)
                    .with_tag("json")
                    .with_tag("smoke"),
            )
    }

    /// Streaming support evaluation (requires the runner to handle streaming).
    pub fn streaming_suite() -> EvalSuite {
        EvalSuite::new("streaming")
            .with_description("Streaming response test")
            .add_case(
                EvalCase::new("stream-basic", "Basic stream output", "streaming")
                    .with_min_length(10)
                    .with_tag("streaming"),
            )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eval_case_substring_pass() {
        let case = EvalCase::new("test", "Test", "expected").with_expected_substring("expected");
        let score = case.evaluate("this contains expected value");
        assert!(score.passed);
        assert!(score.details[0].contains("found"));
    }

    #[test]
    fn test_eval_case_substring_fail() {
        let case =
            EvalCase::new("test", "Test", "expected").with_expected_substring("missing_string");
        let score = case.evaluate("this does not contain it");
        assert!(!score.passed);
        assert!(score.details[0].contains("NOT found"));
    }

    #[test]
    fn test_eval_case_regex_pass() {
        let case = EvalCase::new("test", "Test", "123-456-7890")
            .with_expected_pattern(r"\d{3}-\d{3}-\d{4}");
        let score = case.evaluate("Call me at 123-456-7890");
        assert!(score.passed);
    }

    #[test]
    fn test_eval_case_regex_fail() {
        let case = EvalCase::new("test", "Test", "123-456-7890").with_expected_pattern(r"\d{5}");
        let score = case.evaluate("Call me at 123-456-7890");
        assert!(!score.passed);
    }

    #[test]
    fn test_eval_case_length_checks() {
        let case = EvalCase::new("test", "Test", "short")
            .with_min_length(5)
            .with_max_length(10);
        let score = case.evaluate("hello world this is too long");
        assert!(!score.passed);
        assert!(score.details.iter().any(|d| d.contains("exceeds")));

        let case2 = EvalCase::new("test2", "Test", "ok")
            .with_min_length(1)
            .with_max_length(100);
        let score2 = case2.evaluate("just right");
        assert!(score2.passed);
    }

    #[test]
    fn test_suite_evaluate_all() {
        let suite = EvalSuite::new("test-suite")
            .with_description("Test suite description")
            .add_case(EvalCase::new("c1", "Case 1", "hello").with_expected_substring("hello"))
            .add_case(EvalCase::new("c2", "Case 2", "world").with_expected_substring("world"));

        let result = suite.evaluate_all("test-provider", "hello beautiful world");
        assert_eq!(result.suite, "test-suite");
        assert_eq!(result.provider, "test-provider");
        assert_eq!(result.total, 2);
        assert_eq!(result.passed, 2);
        assert_eq!(result.failed, 0);
        assert!((result.pass_rate - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_suite_evaluate_all_partial_pass() {
        let suite = EvalSuite::new("test-suite")
            .add_case(EvalCase::new("c1", "Case 1", "hello").with_expected_substring("hello"))
            .add_case(EvalCase::new("c2", "Case 2", "world").with_expected_substring("missing"));

        let result = suite.evaluate_all("test-provider", "hello");
        assert_eq!(result.total, 2);
        assert_eq!(result.passed, 1);
        assert_eq!(result.failed, 1);
        assert!((result.pass_rate - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_eval_result_store() {
        let mut store = EvalResultStore::new();
        assert!(store.is_empty());

        let result = EvalSuite::new("test")
            .add_case(EvalCase::new("c1", "Case 1", "hello"))
            .evaluate_all("provider-a", "hello world");

        store.store("provider-a", result);
        assert_eq!(store.len(), 1);
        assert_eq!(store.pass_rate("provider-a", "test"), Some(1.0));
        assert_eq!(store.pass_rate("unknown", "test"), None);
    }

    #[test]
    fn test_builtin_suites() {
        let conn = builtin::connectivity_suite();
        assert_eq!(conn.name, "connectivity");
        assert!(!conn.cases.is_empty());

        let json = builtin::json_output_suite();
        assert_eq!(json.name, "json-output");
        assert!(!json.cases.is_empty());

        let stream = builtin::streaming_suite();
        assert_eq!(stream.name, "streaming");
        assert!(!stream.cases.is_empty());
    }

    #[test]
    fn test_eval_runner_basic() {
        let suite = EvalSuite::new("echo")
            .add_case(EvalCase::new("hello", "Say hello", "hello").with_expected_substring("Hi"));

        // Simulated requester that always returns "Hi there!"
        let requester = |_provider: &str, prompt: &str| -> Result<String, String> {
            assert_eq!(prompt, "hello");
            Ok("Hi there!".to_string())
        };

        let runner = suite.runner().build(requester);
        let result = runner.run("test-provider", |case| case.expected.clone());
        assert_eq!(result.total, 1);
        assert!(result.passed == 1);
    }

    #[test]
    fn test_eval_runner_request_failure() {
        let suite = EvalSuite::new("fail").add_case(EvalCase::new("f1", "Case 1", "prompt"));

        let requester =
            |_: &str, _: &str| -> Result<String, String> { Err("network error".to_string()) };

        let runner = suite.runner().build(requester);
        let result = runner.run("broken-provider", |case| case.expected.clone());
        assert_eq!(result.total, 1);
        assert_eq!(result.passed, 0);
        assert_eq!(result.failed, 1);
        assert!(result.scores[0].details[0].contains("network error"));
    }

    #[test]
    fn test_empty_suite() {
        let suite = EvalSuite::new("empty");
        let result = suite.evaluate_all("provider", "anything");
        assert_eq!(result.total, 0);
        assert_eq!(result.pass_rate, 0.0);
    }
}
