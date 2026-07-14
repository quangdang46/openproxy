import pkg from "../../../package.json" with { type: "json" };

// App configuration
export const APP_CONFIG = {
  name: "OpenProxy",
  description: "OpenProxy dashboard",
  version: pkg.version,
} as const;

// GitHub configuration
export const GITHUB_CONFIG = {
  changelogUrl:
    "https://raw.githubusercontent.com/quangdang46/openproxy/refs/heads/main/CHANGELOG.md",
  repoUrl: "https://github.com/quangdang46/openproxy",
  docsUrl: "https://github.com/quangdang46/openproxy#readme",
  licenseUrl: "https://github.com/quangdang46/openproxy/blob/main/LICENSE",
} as const;

// Updater configuration — binary install via install.sh (not npm)
export const UPDATER_CONFIG = {
  npmPackageName: "openproxy",
  installCmd:
    "curl -fsSL https://raw.githubusercontent.com/quangdang46/openproxy/main/install.sh | bash",
  installCmdLatest:
    "curl -fsSL https://raw.githubusercontent.com/quangdang46/openproxy/main/install.sh | bash",
  exitDelayMs: 500,
  statusPort: 4625,
  statusPollIntervalMs: 1000,
  statusLogTailLines: 8,
  installRetries: 3,
  installRetryDelayMs: 5000,
  lingerAfterDoneMs: 30000,
  waitForExitMinMs: 3000,
  waitForExitMaxMs: 15000,
  waitForExitCheckMs: 500,
  appPort: 4623,
} as const;

// Theme configuration
export const THEME_CONFIG = {
  storageKey: "theme",
  defaultTheme: "system" as const, // "light" | "dark" | "system"
};

// Subscription
export const SUBSCRIPTION_CONFIG = {
  price: 1.0,
  currency: "USD",
  interval: "month" as const,
  planName: "Pro Plan",
} as const;

// API endpoints
export const API_ENDPOINTS = {
  users: "/api/users",
  providers: "/api/providers",
  payments: "/api/payments",
  auth: "/api/auth",
} as const;

export const CONSOLE_LOG_CONFIG = {
  maxLines: 200,
  pollIntervalMs: 1000,
} as const;

// Provider API endpoints (for display only)
export const PROVIDER_ENDPOINTS = {
  openrouter: "https://openrouter.ai/api/v1/chat/completions",
  glm: "https://api.z.ai/api/anthropic/v1/messages",
  "glm-cn": "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
  kimi: "https://api.kimi.com/coding/v1/messages",
  minimax: "https://api.minimax.io/anthropic/v1/messages",
  "minimax-cn": "https://api.minimaxi.com/anthropic/v1/messages",
  alicode: "https://coding.dashscope.aliyuncs.com/v1/chat/completions",
  "alicode-intl": "https://coding-intl.dashscope.aliyuncs.com/v1/chat/completions",
  "volcengine-ark": "https://ark.cn-beijing.volces.com/api/coding/v3/chat/completions",
  byteplus: "https://ark.ap-southeast.bytepluses.com/api/coding/v3/chat/completions",
  openai: "https://api.openai.com/v1/chat/completions",
  anthropic: "https://api.anthropic.com/v1/messages",
  gemini: "https://generativelanguage.googleapis.com/v1beta/models",
  ollama: "https://ollama.com/api/chat",
  "ollama-local": "http://localhost:11434/api/chat",
} as const;

// Re-export from providers.ts for backward compatibility
export {
  FREE_PROVIDERS,
  OAUTH_PROVIDERS,
  APIKEY_PROVIDERS,
  WEB_COOKIE_PROVIDERS,
  AI_PROVIDERS,
  AUTH_METHODS,
} from "./providers";

// Re-export from models.ts for backward compatibility
export {
  PROVIDER_MODELS,
  AI_MODELS,
} from "./models";

/**
 * Quota auto-ping settings contract (9router parity).
 * Persistence: PATCH /api/settings { claudeAutoPing | codexAutoPing }.
 * Scheduler: Rust POST /api/quota/auto-ping/tick (+ boot interval).
 * Full OAuth warm-ping (synthetic 1-token request) is residual.
 */
export const QUOTA_AUTOPING_CONFIG = {
  tickIntervalMs: 60_000,
  pingLeadMs: 5_000,
  providers: {
    claude: {
      settingsKey: "claudeAutoPing",
      quotaKey: "session (5h)",
      pingModel: "claude-haiku-4-5-20251001",
    },
    codex: {
      settingsKey: "codexAutoPing",
      quotaKey: "session",
      pingModel: "gpt-5.5",
    },
  },
} as const;

export const AUTO_PING_SETTINGS_KEYS = {
  claude: "claudeAutoPing",
  codex: "codexAutoPing",
} as const;
