/**
 * Model-aware thinking level picker — port of 9router open-sse/providers/thinkingLevels.js
 * without the open-sse dependency.
 *
 * Resolution order for a (provider, model):
 *   1. reasoning capability required (else null)
 *   2. PATTERN_THINKING model-id overrides (gpt-5.6-sol, *codex*, …)
 *   3. FORMAT_LEVELS[caps.thinkingFormat]
 *   4. base levels ["none","low","medium","high"]
 *   5. strip "none" when thinkingCanDisable === false
 *
 * Capability lookup (thinking-relevant fields only) mirrors open-sse/providers/capabilities.js:
 * provider exact → model exact → glob patterns → defaults (no reasoning).
 */

export type ThinkingFormat =
  | "openai"
  | "claude-adaptive"
  | "claude-budget"
  | "gemini-level"
  | "gemini-budget"
  | "zai"
  | "qwen"
  | "kimi"
  | "deepseek"
  | "minimax"
  | "hunyuan"
  | "step";

export type ThinkingLevel =
  | "none"
  | "minimal"
  | "low"
  | "medium"
  | "high"
  | "xhigh"
  | "max"
  | "thinking"
  | "on"
  | "off"
  | "auto";

export interface ThinkingCaps {
  reasoning: boolean;
  thinkingFormat: ThinkingFormat | null;
  thinkingCanDisable: boolean;
}

const L = {
  base: ["none", "low", "medium", "high"] as ThinkingLevel[],
  onOff: ["none", "thinking"] as ThinkingLevel[],
  openai: ["none", "minimal", "low", "medium", "high", "xhigh"] as ThinkingLevel[],
  levelMax: ["none", "low", "medium", "high", "max"] as ThinkingLevel[],
  budgetX: ["none", "low", "medium", "high", "xhigh", "max"] as ThinkingLevel[],
  gemini: ["minimal", "low", "medium", "high"] as ThinkingLevel[],
  hiMax: ["none", "high", "max"] as ThinkingLevel[],
};

/** thinkingFormat → selectable levels (9router FORMAT_LEVELS). */
const FORMAT_LEVELS: Record<string, ThinkingLevel[]> = {
  openai: L.openai,
  "claude-adaptive": L.levelMax,
  "claude-budget": L.budgetX,
  "gemini-level": L.gemini,
  "gemini-budget": L.base,
  zai: L.onOff,
  qwen: L.base,
  kimi: L.levelMax,
  deepseek: L.hiMax,
  minimax: L.onOff,
  hunyuan: L.base,
  step: L.base,
};

/** Model-id pattern overrides that beat format defaults (9router PATTERN_THINKING). */
const PATTERN_THINKING: Array<{ pattern: string; levels: ThinkingLevel[] }> = [
  {
    pattern: "*gpt-5.6-sol*",
    levels: ["none", "minimal", "low", "medium", "high", "xhigh", "max"],
  },
  { pattern: "*codex*", levels: ["low", "medium", "high", "xhigh"] },
];

const DEFAULT_CAPS: ThinkingCaps = {
  reasoning: false,
  thinkingFormat: null,
  thinkingCanDisable: true,
};

/** Exact model-id overrides (deltas vs DEFAULT). */
const MODEL_CAPS: Record<string, Partial<ThinkingCaps>> = {
  "claude-opus-4.6": { reasoning: true, thinkingFormat: "claude-adaptive" },
  "claude-opus-4.7": { reasoning: true, thinkingFormat: "claude-adaptive" },
  "claude-opus-4-7": { reasoning: true, thinkingFormat: "claude-adaptive" },
  "claude-opus-4.8": { reasoning: true, thinkingFormat: "claude-adaptive" },
  "claude-opus-4-6": { reasoning: true, thinkingFormat: "claude-adaptive" },
  "claude-opus-4-8": { reasoning: true, thinkingFormat: "claude-adaptive" },
  "claude-opus-4.8-thinking": { reasoning: true, thinkingFormat: "claude-adaptive" },
  "claude-opus-4-8-thinking": { reasoning: true, thinkingFormat: "claude-adaptive" },
  "claude-sonnet-4.6": { reasoning: true, thinkingFormat: "claude-adaptive" },
  "claude-sonnet-4-6": { reasoning: true, thinkingFormat: "claude-adaptive" },
  "claude-sonnet-5": { reasoning: true, thinkingFormat: "claude-adaptive" },
  "claude-sonnet-5-thinking": { reasoning: true, thinkingFormat: "claude-adaptive" },
  "claude-sonnet-5-agentic": { reasoning: true, thinkingFormat: "claude-adaptive" },
  "claude-sonnet-5-thinking-agentic": { reasoning: true, thinkingFormat: "claude-adaptive" },
  "glm-4.6v": { reasoning: true, thinkingFormat: "zai" },
  "vision-model": { reasoning: true, thinkingFormat: "qwen" },
  "coder-model": { reasoning: true, thinkingFormat: "qwen" },
};

/** Provider-specific exact model overrides. */
const PROVIDER_CAPS: Record<string, Record<string, Partial<ThinkingCaps>>> = {
  nvidia: {
    "minimaxai/minimax-m2.7": {
      reasoning: true,
      thinkingFormat: "openai",
      thinkingCanDisable: false,
    },
    "minimaxai/minimax-m3": {
      reasoning: true,
      thinkingFormat: "openai",
      thinkingCanDisable: false,
    },
    "z-ai/glm-5.2": { reasoning: true, thinkingFormat: "openai" },
    "deepseek-ai/deepseek-v4-pro": { reasoning: true, thinkingFormat: "openai" },
    "deepseek-ai/deepseek-v4-flash": { reasoning: true, thinkingFormat: "openai" },
  },
  "codebuddy-cn": {
    "glm-5.2": { reasoning: true, thinkingFormat: "openai", thinkingCanDisable: false },
    "glm-5.1": { reasoning: true, thinkingFormat: "openai", thinkingCanDisable: false },
    "glm-5.0": { reasoning: true, thinkingFormat: "openai" },
    "glm-5.0-turbo": { reasoning: true, thinkingFormat: "openai", thinkingCanDisable: false },
    "glm-5v-turbo": { reasoning: true, thinkingFormat: "openai", thinkingCanDisable: false },
    "glm-4.7": { reasoning: true, thinkingFormat: "openai" },
    "minimax-m3": { reasoning: true, thinkingFormat: "openai", thinkingCanDisable: false },
    "minimax-m2.7": { reasoning: true, thinkingFormat: "openai", thinkingCanDisable: false },
    "kimi-k2.7": { reasoning: true, thinkingFormat: "openai", thinkingCanDisable: false },
    "kimi-k2.6": { reasoning: true, thinkingFormat: "openai", thinkingCanDisable: false },
    "kimi-k2.5": { reasoning: true, thinkingFormat: "openai", thinkingCanDisable: false },
    "hy3-preview": { reasoning: true, thinkingFormat: "openai", thinkingCanDisable: false },
    "deepseek-v4-pro": { reasoning: true, thinkingFormat: "openai", thinkingCanDisable: false },
    "deepseek-v4-flash": { reasoning: true, thinkingFormat: "openai", thinkingCanDisable: false },
    "deepseek-v3-2-volc": { reasoning: true, thinkingFormat: "openai", thinkingCanDisable: false },
  },
};

/**
 * Pattern fallback — order matters (specific before generic).
 * Only reasoning / thinkingFormat / thinkingCanDisable deltas are kept.
 */
const PATTERN_CAPS: Array<{ pattern: string; caps: Partial<ThinkingCaps> }> = [
  { pattern: "*claude*opus-4.6*", caps: { reasoning: true, thinkingFormat: "claude-adaptive" } },
  { pattern: "*claude*opus-4.7*", caps: { reasoning: true, thinkingFormat: "claude-adaptive" } },
  { pattern: "*claude*opus-4.8*", caps: { reasoning: true, thinkingFormat: "claude-adaptive" } },
  { pattern: "*claude*sonnet-4.6*", caps: { reasoning: true, thinkingFormat: "claude-adaptive" } },
  { pattern: "*claude*sonnet-4.7*", caps: { reasoning: true, thinkingFormat: "claude-adaptive" } },
  { pattern: "*claude*haiku*", caps: { reasoning: true, thinkingFormat: "claude-budget" } },
  { pattern: "*claude*opus*", caps: { reasoning: true, thinkingFormat: "claude-budget" } },
  { pattern: "*claude*sonnet*", caps: { reasoning: true, thinkingFormat: "claude-budget" } },
  { pattern: "*claude*fable*", caps: { reasoning: true, thinkingFormat: "claude-budget" } },
  { pattern: "*claude*mythos*", caps: { reasoning: true, thinkingFormat: "claude-budget" } },
  { pattern: "*claude*", caps: { reasoning: true, thinkingFormat: "claude-budget" } },

  {
    pattern: "*gemini-3*pro*",
    caps: { reasoning: true, thinkingFormat: "gemini-level", thinkingCanDisable: false },
  },
  {
    pattern: "*gemini-3*",
    caps: { reasoning: true, thinkingFormat: "gemini-level", thinkingCanDisable: false },
  },
  { pattern: "*gemini-2.5*", caps: { reasoning: true, thinkingFormat: "gemini-budget" } },

  { pattern: "*gpt-5*codex*", caps: { reasoning: true, thinkingFormat: "openai" } },
  { pattern: "*gpt-5*", caps: { reasoning: true, thinkingFormat: "openai" } },
  { pattern: "*gpt-oss*", caps: { reasoning: true, thinkingFormat: "openai" } },
  { pattern: "*o1-mini*", caps: { reasoning: true, thinkingFormat: "openai" } },
  { pattern: "*o1*", caps: { reasoning: true, thinkingFormat: "openai" } },
  { pattern: "*o3*", caps: { reasoning: true, thinkingFormat: "openai" } },
  { pattern: "*o4*", caps: { reasoning: true, thinkingFormat: "openai" } },

  { pattern: "*grok-code*", caps: { reasoning: true, thinkingFormat: "openai" } },
  { pattern: "*grok-4.5*", caps: { reasoning: true, thinkingFormat: "openai" } },
  { pattern: "*grok-4*", caps: { reasoning: true, thinkingFormat: "openai" } },
  { pattern: "*grok-3*", caps: { reasoning: true, thinkingFormat: "openai" } },
  { pattern: "*grok*", caps: { reasoning: true, thinkingFormat: "openai" } },

  { pattern: "*qwen*vl*", caps: { reasoning: true, thinkingFormat: "qwen" } },
  { pattern: "*qwen*omni*", caps: { reasoning: true, thinkingFormat: "qwen" } },
  { pattern: "*qwen*coder*", caps: { reasoning: true, thinkingFormat: "qwen" } },
  { pattern: "*qwen*max*", caps: { reasoning: true, thinkingFormat: "qwen" } },
  { pattern: "*qwen3.5*", caps: { reasoning: true, thinkingFormat: "qwen" } },
  { pattern: "*qwen3.6*", caps: { reasoning: true, thinkingFormat: "qwen" } },
  { pattern: "*qwen3.7*", caps: { reasoning: true, thinkingFormat: "qwen" } },
  { pattern: "*qwen*plus*", caps: { reasoning: true, thinkingFormat: "qwen" } },
  { pattern: "*qwen*235b*", caps: { reasoning: true, thinkingFormat: "qwen" } },
  { pattern: "*qwq*", caps: { reasoning: true, thinkingFormat: "qwen", thinkingCanDisable: false } },
  { pattern: "*qwen*", caps: { reasoning: true, thinkingFormat: "qwen" } },

  {
    pattern: "*kimi*k2.7*code*",
    caps: { reasoning: true, thinkingFormat: "kimi", thinkingCanDisable: false },
  },
  { pattern: "*kimi*k2*", caps: { reasoning: true, thinkingFormat: "kimi" } },
  { pattern: "*kimi*", caps: { reasoning: true, thinkingFormat: "kimi" } },

  { pattern: "*glm-5*", caps: { reasoning: true, thinkingFormat: "zai" } },
  { pattern: "*glm-4.7*", caps: { reasoning: true, thinkingFormat: "zai" } },
  { pattern: "*glm-4*", caps: { reasoning: true, thinkingFormat: "zai" } },
  { pattern: "*glm*", caps: { reasoning: true, thinkingFormat: "zai" } },

  { pattern: "*deepseek-v4*", caps: { reasoning: true, thinkingFormat: "deepseek" } },
  {
    pattern: "*reasoner*",
    caps: { reasoning: true, thinkingFormat: "deepseek", thinkingCanDisable: false },
  },
  {
    pattern: "*deepseek-r*",
    caps: { reasoning: true, thinkingFormat: "deepseek", thinkingCanDisable: false },
  },
  // Non-reasoning chat variant must win over the generic *deepseek* family.
  { pattern: "*deepseek-chat*", caps: { reasoning: false } },
  { pattern: "*deepseek*", caps: { reasoning: true, thinkingFormat: "deepseek" } },

  { pattern: "*minimax-m3*", caps: { reasoning: true, thinkingFormat: "minimax" } },
  {
    pattern: "*minimax-m2.7*",
    caps: { reasoning: true, thinkingFormat: "minimax", thinkingCanDisable: false },
  },
  {
    pattern: "*minimax*",
    caps: { reasoning: true, thinkingFormat: "minimax", thinkingCanDisable: false },
  },

  { pattern: "*hunyuan*", caps: { reasoning: true, thinkingFormat: "hunyuan" } },
  { pattern: "hy3*", caps: { reasoning: true, thinkingFormat: "hunyuan" } },
  { pattern: "*step-*", caps: { reasoning: true, thinkingFormat: "step" } },
  { pattern: "*nemotron*", caps: { reasoning: true } },
  { pattern: "*ling-*", caps: { reasoning: true } },
];

/** Glob match (* = wildcard), case-insensitive, full-string. */
export function matchPattern(pattern: string, model: string): boolean {
  if (!model) return false;
  const escaped = pattern
    .toLowerCase()
    .replace(/[.+?^${}()|[\]\\]/g, "\\$&")
    .replace(/\*/g, ".*");
  return new RegExp(`^${escaped}$`).test(model.toLowerCase());
}

/** Resolve thinking-relevant capabilities for a model. */
export function getThinkingCapsForModel(
  provider: string | null | undefined,
  model: string | null | undefined,
): ThinkingCaps {
  if (!model) return { ...DEFAULT_CAPS };

  if (provider && PROVIDER_CAPS[provider]?.[model]) {
    return { ...DEFAULT_CAPS, ...PROVIDER_CAPS[provider][model] };
  }

  const baseModel = model.includes("/") ? model.split("/").pop()! : model;
  if (MODEL_CAPS[baseModel]) return { ...DEFAULT_CAPS, ...MODEL_CAPS[baseModel] };
  if (MODEL_CAPS[model]) return { ...DEFAULT_CAPS, ...MODEL_CAPS[model] };

  for (const { pattern, caps } of PATTERN_CAPS) {
    if (matchPattern(pattern, baseModel) || matchPattern(pattern, model)) {
      return { ...DEFAULT_CAPS, ...caps };
    }
  }

  return { ...DEFAULT_CAPS };
}

/**
 * Valid thinking levels for a provider/model, or null when the model has no
 * reasoning capability (picker should not offer a suffix for it).
 *
 * Mirrors 9router getThinkingLevels(provider, model).
 */
export function getThinkingLevels(
  provider: string | null | undefined,
  model: string | null | undefined,
): ThinkingLevel[] | null {
  if (!model) return null;
  const caps = getThinkingCapsForModel(provider, model);
  if (!caps.reasoning) return null;

  const hit = PATTERN_THINKING.find((p) => matchPattern(p.pattern, model));
  let levels: ThinkingLevel[] = hit?.levels
    ? [...hit.levels]
    : caps.thinkingFormat && FORMAT_LEVELS[caps.thinkingFormat]
      ? [...FORMAT_LEVELS[caps.thinkingFormat]]
      : [...L.base];

  if (caps.thinkingCanDisable === false) {
    levels = levels.filter((l) => l !== "none");
  }
  return levels;
}

/**
 * Union of thinking levels across model ids for a provider-level picker.
 * Always prefixes "auto". Returns null when no reasoning models are present
 * (caller may fall back to THINKING_CONFIG).
 */
export function unionThinkingLevels(
  provider: string | null | undefined,
  modelIds: Iterable<string>,
): ThinkingLevel[] | null {
  const set = new Set<ThinkingLevel>();
  const seen = new Set<string>();
  for (const modelId of modelIds) {
    if (!modelId || seen.has(modelId)) continue;
    seen.add(modelId);
    const lv = getThinkingLevels(provider, modelId);
    if (!lv) continue;
    for (const l of lv) {
      if (l !== "none") set.add(l);
    }
  }
  if (set.size === 0) return null;
  // Stable-ish order: known ladder first, then any leftovers.
  const order: ThinkingLevel[] = [
    "minimal",
    "low",
    "medium",
    "high",
    "xhigh",
    "max",
    "thinking",
    "on",
    "off",
  ];
  const ordered = order.filter((l) => set.has(l));
  for (const l of set) {
    if (!ordered.includes(l)) ordered.push(l);
  }
  return ["auto", ...ordered];
}

/**
 * Map coarse THINKING_CONFIG options into picker levels (fallback path).
 * Drops "auto" (handled by the UI separately) and normalizes "off" → "none"
 * so suffix copy matches backend strip/re-apply conventions.
 */
export function levelsFromThinkingConfig(options: string[] | null | undefined): ThinkingLevel[] | null {
  if (!options?.length) return null;
  const out: ThinkingLevel[] = [];
  const seen = new Set<string>();
  for (const raw of options) {
    if (!raw || raw === "auto") continue;
    const mapped = (raw === "off" ? "none" : raw) as ThinkingLevel;
    if (seen.has(mapped)) continue;
    seen.add(mapped);
    out.push(mapped);
  }
  return out.length ? out : null;
}
