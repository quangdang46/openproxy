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
  model?: string;
  provider?: string;
  caps?: ModelCaps;
}

/**
 * Fetch model capabilities once and expose a lookup by fullModel
 * ("provider/model") or bare model id. Falls back to a client-side pattern
 * heuristic when `/api/models` does not include `caps` (OpenProxy today).
 */
export function useModelCaps() {
  const [byFull, setByFull] = useState<Record<string, ModelCaps>>({});
  const [byId, setById] = useState<Record<string, ModelCaps>>({});

  useEffect(() => {
    let alive = true;
    (async () => {
      try {
        const res = await fetch("/api/models", { cache: "no-store" });
        if (!res.ok) return;
        const data = await res.json();
        const full: Record<string, ModelCaps> = {};
        const id: Record<string, ModelCaps> = {};
        for (const m of (data.models || []) as ModelsApiEntry[]) {
          let caps = m.caps;
          if (!caps || (typeof caps === "object" && !Object.values(caps).some(Boolean))) {
            // Backend may omit caps — infer so badges still light up.
            caps = inferCaps(m.provider || null, m.model || "");
          }
          if (!caps) continue;
          if (m.fullModel) full[m.fullModel] = caps;
          if (m.model) id[m.model] = caps;
        }
        if (alive) {
          setByFull(full);
          setById(id);
        }
      } catch {
        /* ignore — getCaps still has heuristic fallback */
      }
    })();
    return () => {
      alive = false;
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
