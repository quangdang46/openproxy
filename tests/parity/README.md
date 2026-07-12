# Parity tests (9router behavioral locks)

Unit locks live under `src/**` with `mod parity_tests` / `#[cfg(test)]` so `cargo test --lib` is enough for CI (no network).

## Run

```bash
./scripts/parity-smoke.sh
# or individually:
cargo test -p openproxy --lib stream_flags
cargo test -p openproxy --lib parity_tests
cargo test -p openproxy --lib chat::
cargo test -p openproxy --lib combo
```

## Decision logging

Use `RUST_LOG=openproxy::chat=debug,openproxy::fusion=debug,openproxy::github=debug` when debugging live.

## Covered matrices

| Area | Where |
|------|--------|
| detect_format / direct routes | `translator/registry.rs` `parity_tests` |
| stream flags / forceStream | `utils/stream_flags.rs` |
| RequestPlan transport / anthropic-compatible | `core/chat/mod.rs` tests |
| combo RR / fusion grace | `core/combo/*` |
| ERROR_RULES → check_fallback_error | `config/error_config.rs` + combo |

## Live (optional)

`OPENPROXY_LIVE_PARITY=1` reserved for future e2e; not required for CI.
