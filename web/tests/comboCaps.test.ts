/**
 * Unit tests for web/src/shared/utils/comboCaps.ts — the fmtK formatter
 * behind the combos page aggregated capability envelope (9router commit
 * 5c217d3). Run with:
 *
 *   node --test web/tests/comboCaps.test.ts
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { fmtK } from "../src/shared/utils/comboCaps.ts";

test("fmtK renders unknown / zero as ?", () => {
  assert.equal(fmtK(0), "?");
  assert.equal(fmtK(undefined), "?");
  assert.equal(fmtK(null), "?");
});

test("fmtK renders millions with at most one decimal", () => {
  assert.equal(fmtK(1_000_000), "1M");
  assert.equal(fmtK(1_500_000), "1.5M");
  assert.equal(fmtK(2_400_000), "2.4M");
});

test("fmtK renders thousands rounded", () => {
  assert.equal(fmtK(200_000), "200k");
  assert.equal(fmtK(131_072), "131k");
  assert.equal(fmtK(999), "1k");
});
