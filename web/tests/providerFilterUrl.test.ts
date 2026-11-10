/**
 * Unit tests for web/src/shared/utils/providerFilterUrl.ts — the ?provider=
 * deep-link helpers behind the quota page (9router commit 65826d9, #4217).
 * Run with:
 *
 *   node --test web/tests/providerFilterUrl.test.ts
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  providerFilterFromSearch,
  urlWithProviderParam,
} from "../src/shared/utils/providerFilterUrl.ts";

test("providerFilterFromSearch reads the provider param", () => {
  assert.equal(providerFilterFromSearch("?provider=claude"), "claude");
  assert.equal(providerFilterFromSearch("provider=codex&foo=1"), "codex");
});

test("providerFilterFromSearch falls back to all", () => {
  assert.equal(providerFilterFromSearch(""), "all");
  assert.equal(providerFilterFromSearch("?foo=1"), "all");
  assert.equal(providerFilterFromSearch("?provider="), "all");
  assert.equal(providerFilterFromSearch("?provider=%20"), "all");
});

test("urlWithProviderParam sets and preserves other params", () => {
  assert.equal(
    urlWithProviderParam("/dashboard/quota", "?tab=x", "claude"),
    "/dashboard/quota?tab=x&provider=claude",
  );
});

test("urlWithProviderParam removes provider when filter is all", () => {
  assert.equal(
    urlWithProviderParam("/dashboard/quota", "?provider=claude&tab=x", "all"),
    "/dashboard/quota?tab=x",
  );
  assert.equal(
    urlWithProviderParam("/dashboard/quota", "?provider=claude", "all"),
    "/dashboard/quota",
  );
});
