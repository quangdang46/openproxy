//! Usage tracking and cost calculation.
//!
//! This module provides:
//! - [`PricingTable`](pricing::PricingTable): Per-model pricing rates
//! - [`UsageTracker`](tracker::UsageTracker): Tracks request/response usage and calculates costs
//!
//! ## Default Pricing (from README)
//!
//! | Provider | Model | Cost | Reset |
//! |----------|-------|------|--------|
//! | GLM | glm-5.1, glm-4.7 | $0.6/1M tokens | Daily 10AM |
//! | MiniMax | MiniMax-M2.7 | $0.2/1M tokens | 5-hour rolling |
//! | Kimi | kimi-k2.5 | $9/mo flat | Monthly |
//! | Kiro | all | FREE | Unlimited |
//! | OpenCode | all | FREE | Unlimited |
//! | Vertex | all | $300 credits | New GCP accounts |
//!
//! Subscriptions (Claude Code, Codex, Copilot, Cursor) are tracked via their own quota systems.

pub mod grok_cli_quota_frame;
mod pricing;
pub mod quota_fetcher;
pub mod tracker;

pub(crate) use pricing::parse_model_pricing;
pub use pricing::{CostModel, ModelPricing, Pricing};
pub use tracker::{CompressionStats, DailyUsageSummary, ProviderUsage, UsageSummary, UsageTracker};
