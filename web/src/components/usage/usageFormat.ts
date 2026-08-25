// Shared formatting + visual constants for the OmniRoute-style usage pages
// (Provider Breakdown, Usage Analytics, Compression).
//
// Colors mirror the OmniRoute v3.8.50 screenshots (rose/emerald/amber) for the
// data columns so the pages match the provided visual reference, while page
// chrome (cards, borders, text) keeps using the dashboard design tokens
// (bg-surface-2, border-border, text-text-muted, ...) so it stays consistent
// with the rest of the OpenProxy dashboard.

export const USAGE_COLORS = {
  input: "#f43f5e", // rose-500  — INPUT tokens
  output: "#10b981", // emerald-500 — OUTPUT tokens
  total: "#faf9f5", // near-white — TOTAL (bold)
  cost: "#f59e0b", // amber-500 — COST
  active: "#ef4444", // red-500 — active period pill
  violet: "#a855f7", // violet — links / ratios
  cyan: "#22d3ee", // cyan — avg tokens/req, diversity
  pink: "#ec4899", // pink — highlights
  teal: "#2dd4bf", // teal — top provider
  heatLow: "#3a1717",
  heatHigh: "#ef4444",
} as const;

// Provider brand colors (OmniRoute-style). Falls back to a rotating palette.
const PROVIDER_COLORS: Record<string, string> = {
  openrouter: "#f59e0b",
  kiro: "#f43f5e",
  openai: "#10a37f",
  anthropic: "#d97757",
  claude: "#d97757",
  gemini: "#4285f4",
  google: "#4285f4",
  groq: "#f55036",
  grok: "#111827",
  xai: "#111827",
  deepseek: "#4d6bfe",
  mistral: "#ff7000",
  perplexity: "#20808a",
  together: "#0b1f3a",
  fireworks: "#ff6b00",
  cerebras: "#1a73e8",
  cohere: "#39594d",
  nvidia: "#76b900",
  siliconflow: "#7c3aed",
  nebius: "#0ea5e9",
  chutes: "#a855f7",
  hyperbolic: "#22d3ee",
  glm: "#5b8def",
  "glm-cn": "#5b8def",
  minimax: "#7c3aed",
  kimi: "#111827",
  qwen: "#615ced",
  ollama: "#10b981",
  opencode: "#38bdf8",
  "vertex-ai": "#4285f4",
  copilot: "#6e5494",
  cursor: "#111827",
  codex: "#10a37f",
  antigravity: "#a855f7",
  venice: "#f59e0b",
  infomaniak: "#0ea5e9",
  scaleway: "#ffffff",
  "llm7": "#22d3ee",
  nvidia_nim: "#76b900",
};

const FALLBACK_PALETTE = [
  "#f59e0b",
  "#f43f5e",
  "#10b981",
  "#3b82f6",
  "#a855f7",
  "#06b6d4",
  "#eab308",
  "#ec4899",
  "#84cc16",
  "#f97316",
  "#14b8a6",
  "#8b5cf6",
];

export function providerColor(provider: string, index = 0): string {
  const key = (provider || "").toLowerCase();
  if (PROVIDER_COLORS[key]) return PROVIDER_COLORS[key];
  return FALLBACK_PALETTE[index % FALLBACK_PALETTE.length];
}

const PROVIDER_NAMES: Record<string, string> = {
  openrouter: "OpenRouter",
  kiro: "Kiro",
  openai: "OpenAI",
  anthropic: "Anthropic",
  claude: "Claude",
  gemini: "Gemini",
  google: "Google",
  groq: "Groq",
  grok: "Grok",
  xai: "xAI",
  deepseek: "DeepSeek",
  mistral: "Mistral",
  perplexity: "Perplexity",
  together: "Together",
  fireworks: "Fireworks",
  cerebras: "Cerebras",
  cohere: "Cohere",
  nvidia: "NVIDIA",
  siliconflow: "SiliconFlow",
  nebius: "Nebius",
  chutes: "Chutes",
  hyperbolic: "Hyperbolic",
  glm: "GLM",
  "glm-cn": "GLM",
  minimax: "MiniMax",
  kimi: "Kimi",
  qwen: "Qwen",
  ollama: "Ollama",
  opencode: "OpenCode",
  "vertex-ai": "Vertex AI",
  copilot: "GitHub Copilot",
  cursor: "Cursor",
  codex: "Codex",
  antigravity: "Antigravity",
  venice: "Venice",
  infomaniak: "Infomaniak",
  scaleway: "Scaleway",
  "llm7": "LLM7",
  nvidia_nim: "NVIDIA NIM",
};

export function providerName(provider: string): string {
  const key = (provider || "").toLowerCase();
  if (PROVIDER_NAMES[key]) return PROVIDER_NAMES[key];
  if (!provider) return "Unknown";
  return provider.charAt(0).toUpperCase() + provider.slice(1);
}

// 1.2M / 48.5K style compact token formatting.
export function formatTokens(n: number): string {
  const v = n || 0;
  const abs = Math.abs(v);
  if (abs >= 1_000_000_000) return trimZero(v / 1_000_000_000) + "B";
  if (abs >= 1_000_000) return trimZero(v / 1_000_000) + "M";
  if (abs >= 1_000) return trimZero(v / 1_000) + "K";
  return String(v);
}

function trimZero(v: number): string {
  const s = v.toFixed(1);
  return s.endsWith(".0") ? s.slice(0, -2) : s;
}

export function formatNumber(n: number): string {
  return new Intl.NumberFormat().format(Math.round(n || 0));
}

export function formatCost(n: number): string {
  return "$" + (n || 0).toFixed(2);
}

export function formatPercent(n: number, digits = 1): string {
  return (n || 0).toFixed(digits) + "%";
}

export function formatDuration(ms: number): string {
  if (ms == null || isNaN(ms)) return "—";
  if (ms < 1) return "<1ms";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  return `${(ms / 1000).toFixed(2)}s`;
}

// Map a UI period label to a backend UsagePeriod value.
export function periodToBackend(label: string): string {
  switch (label) {
    case "1D":
    case "24h":
      return "today";
    case "7D":
      return "7d";
    case "30D":
      return "30d";
    case "90D":
      return "60d"; // backend max bounded window
    case "YTD":
    case "All":
    case "all":
      return "all";
    default:
      return "30d";
  }
}
