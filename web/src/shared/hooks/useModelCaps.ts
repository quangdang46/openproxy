"use client";

import { useState, useEffect, useCallback } from "react";
import type { ModelCaps } from "@/shared/constants/models";

/**
 * Lightweight client-side capability heuristic for models not present in the
 * `/api/models` response (or when the backend doesn't yet emit `caps`).
 * Covers the badges we actually render (vision / reasoning). Pattern order is
 * specific → generic, case-insensitive full-id match.
 */
function matchGlob(pattern: string, value: string): boolean {
  // Convert simple * globs to a fully-anchored regex.
  const escaped = pattern
    .toLowerCase()
    .replace(/[.+?^${}()|[\]\\]/g, "\\$&")
    .replace(/\*/g, ".*");
  return new RegExp(`^${escaped}$`).test(value.toLowerCase());
}

const PATTERN_CAPS: Array<{ pattern: string; caps: ModelCaps }> = [
  // Claude
  { pattern: "*claude*", caps: { vision: true, reasoning: true } },
  // Gemini / Gemma
  { pattern: "*gemini*", caps: { vision: true, reasoning: true } },
  { pattern: "*gemma*", caps: { vision: true } },
  // OpenAI GPT / o-series
  { pattern: "*gpt-5*codex*", caps: { reasoning: true } },
  { pattern: "*gpt-5*", caps: { vision: true, reasoning: true } },
  { pattern: "*gpt-4o*", caps: { vision: true } },
  { pattern: "*gpt-4.1*", caps: { vision: true } },
  { pattern: "*gpt-4-turbo*", caps: { vision: true } },
  { pattern: "*gpt-oss*", caps: { reasoning: true } },
  { pattern: "*o1-mini*", caps: { reasoning: true } },
  { pattern: "*o1*", caps: { vision: true, reasoning: true } },
  { pattern: "*o3*", caps: { vision: true, reasoning: true } },
  { pattern: "*o4*", caps: { vision: true, reasoning: true } },
  // Grok
  { pattern: "*grok-code*", caps: { reasoning: true } },
  { pattern: "*grok*", caps: { vision: true, reasoning: true } },
  // Qwen
  { pattern: "*qwen*vl*", caps: { vision: true, reasoning: true } },
  { pattern: "*qwen*omni*", caps: { vision: true, reasoning: true } },
  { pattern: "*qwen3.5*", caps: { vision: true, reasoning: true } },
  { pattern: "*qwen3.6*", caps: { vision: true, reasoning: true } },
  { pattern: "*qwen3.7*", caps: { vision: true, reasoning: true } },
  { pattern: "*qwen*plus*", caps: { vision: true, reasoning: true } },
  { pattern: "*qwen*", caps: { reasoning: true } },
  { pattern: "*qwq*", caps: { reasoning: true } },
  // Kimi
  { pattern: "*kimi*", caps: { vision: true, reasoning: true } },
  // GLM / Z.ai
  { pattern: "*glm*v*", caps: { vision: true, reasoning: true } },
  { pattern: "*glm*", caps: { reasoning: true } },
  // DeepSeek
  { pattern: "*deepseek*r1*", caps: { reasoning: true } },
  { pattern: "*deepseek*v3*", caps: { reasoning: true } },
  { pattern: "*deepseek*v4*", caps: { vision: true, reasoning: true } },
  { pattern: "*deepseek*", caps: { reasoning: true } },
  // MiniMax
  { pattern: "*minimax*", caps: { vision: true, reasoning: true } },
  // Mistral vision-ish
  { pattern: "*pixtral*", caps: { vision: true } },
  { pattern: "*mistral*small*", caps: { vision: true } },
  // Generic name hints
  { pattern: "*vision*", caps: { vision: true } },
  { pattern: "*-vl*", caps: { vision: true } },
  { pattern: "*reasoning*", caps: { reasoning: true } },
];

function inferCaps(provider: string | null, modelId: string): ModelCaps {
  const candidates = [modelId];
  if (provider) candidates.push(`${provider}/${modelId}`);
  for (const candidate of candidates) {
    for (const rule of PATTERN_CAPS) {
      if (matchGlob(rule.pattern, candidate)) {
        return { ...rule.caps };
      }
    }
  }
  return {};
}

interface ModelsApiEntry {
  fullModel?: string;
  routedModel?: string;
  model?: string;
  provider?: string;
  caps?: ModelCaps;
}

type CapsMaps = { byFull: Record<string, ModelCaps>; byId: Record<string, ModelCaps> };

// Module cache: one /api/models fetch shared by every useModelCaps instance.
// Five components mount this hook; without it each one refetches on every mount.
let cache: CapsMaps | null = null;
let inflight: Promise<CapsMaps> | null = null;

function buildMaps(models: ModelsApiEntry[]): CapsMaps {
  const byFull: Record<string, ModelCaps> = {};
  const byId: Record<string, ModelCaps> = {};
  for (const m of models || []) {
    let caps = m.caps;
    if (!caps || (typeof caps === "object" && !Object.values(caps).some(Boolean))) {
      // Backend may omit caps — infer so badges still light up.
      caps = inferCaps(m.provider || null, m.model || "");
    }
    if (!caps) continue;
    if (m.fullModel) byFull[m.fullModel] = caps;
    // A routed model is `providerAlias/model`; clients that already resolved the
    // provider alias look it up under that key.
    if (m.routedModel) byFull[m.routedModel] = caps;
    if (m.model) byId[m.model] = caps;
  }
  return { byFull, byId };
}

function loadModelCaps(): Promise<CapsMaps> {
  if (cache) return Promise.resolve(cache);
  if (inflight) return inflight;
  inflight = fetch("/api/models", { cache: "no-store" })
    .then((res) => {
      if (!res.ok) throw new Error(`models ${res.status}`);
      return res.json();
    })
    .then((data) => {
      cache = buildMaps((data.models || []) as ModelsApiEntry[]);
      return cache;
    })
    .catch(() => {
      // Keep the cache null so a later mount can retry.
      return { byFull: {}, byId: {} };
    })
    .finally(() => {
      inflight = null;
    });
  return inflight;
}

/**
 * Fetch model capabilities once and expose a lookup by fullModel
 * ("provider/model") or bare model id. Falls back to a client-side pattern
 * heuristic when `/api/models` does not include `caps` (OpenProxy today).
 */
export function useModelCaps() {
  const [byFull, setByFull] = useState<Record<string, ModelCaps>>(() => cache?.byFull || {});
  const [byId, setById] = useState<Record<string, ModelCaps>>(() => cache?.byId || {});

  useEffect(() => {
    let alive = true;
    const sync = (maps: CapsMaps): void => {
      if (alive) {
        setByFull(maps.byFull);
        setById(maps.byId);
      }
    };
    if (cache) sync(cache);
    else loadModelCaps().then(sync);

    // Custom models are added and removed at runtime; drop the shared cache so
    // every mounted consumer sees the new catalog.
    const invalidate = (): void => {
      cache = null;
      loadModelCaps().then(sync);
    };
    window.addEventListener("customModelChanged", invalidate);
    return () => {
      alive = false;
      window.removeEventListener("customModelChanged", invalidate);
    };
  }, []);

  const getCaps = useCallback(
    (key: string | null | undefined): ModelCaps | null => {
      if (!key) return null;
      if (byFull[key]) return byFull[key];
      const bare = key.includes("/") ? key.slice(key.indexOf("/") + 1) : key;
      if (byId[bare]) return byId[bare];
      // Fallback: compute caps for dynamic models not in static list
      const provider = key.includes("/") ? key.slice(0, key.indexOf("/")) : null;
      return inferCaps(provider, bare);
    },
    [byFull, byId],
  );

  return { getCaps };
}
