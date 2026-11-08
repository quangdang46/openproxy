/**
 * Unit tests for web/src/shared/utils/capacityAdapterPool.ts — the ordered
 * pool helpers behind the combos page Vision Adapter (9router commit
 * 3e4323e). Run with:
 *
 *   node --test web/tests/capacityAdapterPool.test.ts
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  addPoolModel,
  deselectPoolModel,
  removePoolModelAt,
  resolvePoolModelValue,
} from "../src/shared/utils/capacityAdapterPool.ts";

const FALLBACK = "oc/mimo-v2.5-free";

test("resolvePoolModelValue accepts Model objects and bare strings", () => {
  assert.equal(resolvePoolModelValue({ value: "oc/a", name: "A" }), "oc/a");
  assert.equal(resolvePoolModelValue({ name: "oc/b" }), "oc/b");
  assert.equal(resolvePoolModelValue("oc/c"), "oc/c");
  assert.equal(resolvePoolModelValue(null), "");
  assert.equal(resolvePoolModelValue({}), "");
});

test("addPoolModel appends in order and dedupes", () => {
  assert.deepEqual(addPoolModel([], { value: "oc/a" }), ["oc/a"]);
  assert.deepEqual(addPoolModel(["oc/a"], { value: "oc/b" }), ["oc/a", "oc/b"]);
  assert.deepEqual(addPoolModel(["oc/a"], { value: "oc/a" }), ["oc/a"]);
  assert.deepEqual(addPoolModel(["oc/a"], ""), ["oc/a"]);
});

test("deselectPoolModel removes the model from the combo pool", () => {
  const pool = ["oc/a", "oc/b", "oc/c"];
  assert.deepEqual(deselectPoolModel(pool, { value: "oc/b" }, FALLBACK), [
    "oc/a",
    "oc/c",
  ]);
  // Unknown value leaves the pool untouched.
  assert.deepEqual(deselectPoolModel(pool, { value: "oc/zzz" }, FALLBACK), pool);
});

test("deselectPoolModel falls back to the default when the pool empties", () => {
  assert.deepEqual(deselectPoolModel(["oc/a"], "oc/a", FALLBACK), [FALLBACK]);
  assert.deepEqual(deselectPoolModel([FALLBACK], FALLBACK, FALLBACK), [FALLBACK]);
});

test("removePoolModelAt removes by index with the same empty-pool fallback", () => {
  assert.deepEqual(removePoolModelAt(["oc/a", "oc/b"], 0, FALLBACK), ["oc/b"]);
  assert.deepEqual(removePoolModelAt(["oc/a"], 0, FALLBACK), [FALLBACK]);
});
