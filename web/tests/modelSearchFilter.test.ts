/**
 * Unit tests for web/src/shared/utils/modelSearchFilter.ts — a port of 9router
 * src/shared/utils/modelSearchFilter.js. Run with:
 *
 *   node --test web/tests/modelSearchFilter.test.ts
 *
 * (Node >= 22.6 strips TS types natively; no vitest dependency needed.)
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  parseSearchQuery,
  matchesSearch,
  filterModelsByQuery,
} from "../src/shared/utils/modelSearchFilter.ts";

test("parseSearchQuery: empty / nullish input", () => {
  for (const q of [null, undefined, "", "   "]) {
    const p = parseSearchQuery(q);
    assert.equal(p.isEmpty, true);
    assert.equal(p.isRegex, false);
    assert.equal(p.regex, null);
    assert.deepEqual(p.positiveTerms, []);
    assert.deepEqual(p.negativeTerms, []);
  }
});

test("parseSearchQuery: plain substring terms", () => {
  const p = parseSearchQuery("Claude Sonnet");
  assert.equal(p.isEmpty, false);
  assert.equal(p.isRegex, false);
  assert.deepEqual(p.positiveTerms, ["claude", "sonnet"]);
  assert.deepEqual(p.negativeTerms, []);
});

test("parseSearchQuery: ! and - negation tokens", () => {
  const p = parseSearchQuery("claude !haiku -mini");
  assert.deepEqual(p.positiveTerms, ["claude"]);
  assert.deepEqual(p.negativeTerms, ["haiku", "mini"]);
});

test("parseSearchQuery: lone ! or - is a positive term", () => {
  const p = parseSearchQuery("!");
  assert.deepEqual(p.positiveTerms, ["!"]);
  assert.deepEqual(p.negativeTerms, []);
});

test("parseSearchQuery: regex with flags", () => {
  const p = parseSearchQuery("/^claude-.*$/i");
  assert.equal(p.isRegex, true);
  assert.ok(p.regex instanceof RegExp);
  assert.equal(p.regex!.flags, "i");
});

test("parseSearchQuery: regex without closing slash (live typing)", () => {
  const p = parseSearchQuery("/claude.*");
  assert.equal(p.isRegex, true);
  assert.ok(p.regex!.test("claude-opus"));
});

test("parseSearchQuery: invalid regex falls back to literal terms", () => {
  // "/(/" cannot compile; upstream falls through to term parsing.
  const p = parseSearchQuery("/(/");
  // Either isRegex with a working regex or literal — must not throw.
  assert.doesNotThrow(() => matchesSearch("anything", p));
});

test("matchesSearch: empty query matches everything", () => {
  assert.equal(matchesSearch("gpt-4o", ""), true);
  assert.equal(matchesSearch({ name: "x" }, parseSearchQuery("  ")), true);
});

test("matchesSearch: substring on object candidates (name/id/value)", () => {
  const m = { id: "gpt-4o", name: "GPT-4o", value: "openai/gpt-4o" };
  assert.equal(matchesSearch(m, "gpt-4o"), true);
  assert.equal(matchesSearch(m, "GPT"), true); // case-insensitive
  assert.equal(matchesSearch(m, "claude"), false);
});

test("matchesSearch: all positive terms must match", () => {
  const m = { id: "claude-sonnet-4", name: "Claude Sonnet 4", value: "anthropic/claude-sonnet-4" };
  assert.equal(matchesSearch(m, "claude sonnet"), true);
  assert.equal(matchesSearch(m, "claude opus"), false);
});

test("matchesSearch: negative term rejects", () => {
  const haiku = { id: "claude-haiku", name: "Claude Haiku", value: "anthropic/claude-haiku" };
  const sonnet = { id: "claude-sonnet", name: "Claude Sonnet", value: "anthropic/claude-sonnet" };
  assert.equal(matchesSearch(haiku, "claude !haiku"), false);
  assert.equal(matchesSearch(sonnet, "claude !haiku"), true);
  // -token exclusion works the same
  assert.equal(matchesSearch(haiku, "claude -haiku"), false);
});

test("matchesSearch: negation-only query", () => {
  const embed = { id: "text-embed", name: "Embeddings", value: "openai/text-embed" };
  const chat = { id: "gpt-4o", name: "GPT-4o", value: "openai/gpt-4o" };
  assert.equal(matchesSearch(embed, "!embed"), false);
  assert.equal(matchesSearch(chat, "!embed"), true);
});

test("matchesSearch: regex query", () => {
  assert.equal(matchesSearch("claude-opus-4-1", "/^claude-opus/"), true);
  assert.equal(matchesSearch("gpt-4o", "/^claude/"), false);
  assert.equal(matchesSearch("GPT-4O", "/gpt-4o$/"), true);
});

test("matchesSearch: regex against object uses joined candidates", () => {
  const m = { id: "foo", name: "Bar", value: "baz/qux" };
  assert.equal(matchesSearch(m, "/foo.*qux/"), true);
});

test("matchesSearch: regex negative assertion tests full text only", () => {
  const m = { id: "abc", name: "def", value: "ghi" };
  // (?!...) forces the full-candidate-text branch upstream.
  assert.equal(matchesSearch(m, "/(?!xyz)abc/"), true);
});

test("matchesSearch: reused regex resets lastIndex between targets (v0.5.109 #5853a88)", () => {
  for (const flags of ["g", "y", "gy"]) {
    const query = parseSearchQuery(`/(?<!sonnet)claude/${flags}`);
    assert.equal(matchesSearch("claude-opus", query), true);
    assert.equal(matchesSearch("claude-sonnet", query), true);
    assert.equal(matchesSearch("claude-haiku", query), true);
  }
});

test("matchesSearch: preserves case-sensitive and sticky matching semantics", () => {
  const query = parseSearchQuery("/claude/y");
  assert.equal(matchesSearch("Claude-opus", query), false);
  assert.equal(matchesSearch("anthropic/claude-opus", query), false);
  assert.equal(matchesSearch("claude-opus", query), true);
});

test("matchesSearch: string[] target", () => {
  assert.equal(matchesSearch(["alpha", "beta"], "beta"), true);
  assert.equal(matchesSearch(["alpha", "beta"], "gamma"), false);
});

test("matchesSearch: empty candidates return false for non-empty query", () => {
  assert.equal(matchesSearch({}, "x"), false);
  assert.equal(matchesSearch(null, "x"), false);
});

test("filterModelsByQuery: filters array, preserves empty-query identity", () => {
  const items = [
    { id: "a", name: "Claude Sonnet", value: "anthropic/claude-sonnet" },
    { id: "b", name: "GPT-4o", value: "openai/gpt-4o" },
    { id: "c", name: "Claude Haiku", value: "anthropic/claude-haiku" },
  ];
  assert.equal(filterModelsByQuery(items, ""), items);
  assert.deepEqual(
    filterModelsByQuery(items, "claude !haiku").map((m) => m.id),
    ["a"],
  );
  assert.deepEqual(
    filterModelsByQuery(items, "/^gpt/").map((m) => m.id),
    ["b"],
  );
  assert.deepEqual(filterModelsByQuery([], "x"), []);
  assert.deepEqual(filterModelsByQuery(null, "x"), []);
});

for (const [rawQuery, names] of [
  ["/(?<!sonnet)claude/g", ["claude-opus", "claude-sonnet", "claude-haiku", "claude-4.5"]],
  ["/^(?!.*haiku).*claude/g", ["claude-opus", "claude-sonnet", "claude-4.5"]],
] as const) {
  test(`filterModelsByQuery: keeps global assertion search ${rawQuery} independent of item order and previous filters`, () => {
    const models = ["claude-opus", "claude-sonnet", "claude-haiku", "claude-4.5"]
      .map((name) => ({ name, id: `anthropic/${name}` }));
    const expected = models.filter((m) => (names as readonly string[]).includes(m.name));
    const query = parseSearchQuery(rawQuery);

    assert.deepEqual(filterModelsByQuery(models, query), expected);
    assert.deepEqual(filterModelsByQuery(models, query), expected);
    assert.deepEqual(filterModelsByQuery([...models].reverse(), query), [...expected].reverse());
  });
}
