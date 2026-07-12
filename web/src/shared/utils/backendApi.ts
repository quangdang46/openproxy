/**
 * Helper functions for calling the OpenProxy Rust backend API from the
 * Astro/React dashboard. Prefer relative `/api/*` URLs in the browser so
 * session cookies and the active host/port always match.
 */

function apiBase(): string {
  if (typeof window !== "undefined") {
    // Same-origin relative paths — preserves dashboard session cookie.
    return "";
  }
  return (
    process.env.NEXT_PUBLIC_BASE_URL ??
    process.env.BASE_URL ??
    "http://127.0.0.1:4623"
  );
}

function apiUrl(path: string): string {
  const base = apiBase();
  if (!base) return path;
  return `${base.replace(/\/$/, "")}${path}`;
}

async function apiFetch(path: string, init: RequestInit = {}): Promise<Response> {
  const headers = new Headers(init.headers);
  if (init.body && !headers.has("Content-Type")) {
    headers.set("Content-Type", "application/json");
  }
  return fetch(apiUrl(path), {
    ...init,
    headers,
    credentials: "same-origin",
  });
}

export interface Settings {
  tunnelEnabled?: boolean;
  tailscaleEnabled?: boolean;
  cloudEnabled?: boolean;
  mitmEnabled?: boolean;
  claudeAutoPing?: AutoPingConfig;
  codexAutoPing?: AutoPingConfig;
  [key: string]: unknown;
}

export interface AutoPingConfig {
  enabled?: boolean;
  connections?: Record<string, boolean>;
}

interface ApiKey {
  id: string;
  name: string;
  key: string;
  createdAt: string;
  [key: string]: unknown;
}

interface TunnelStatus {
  enabled?: boolean;
  url?: string;
  tunnel?: { running?: boolean; enabled?: boolean; tunnelUrl?: string };
  tailscale?: { running?: boolean; enabled?: boolean; tunnelUrl?: string };
  [key: string]: unknown;
}

interface ConsoleLog {
  timestamp: string;
  level: string;
  message: string;
  [key: string]: unknown;
}

interface MitmConfig {
  enabled: boolean;
  port?: number;
  [key: string]: unknown;
}

/**
 * Get settings from the Rust backend
 */
export async function getSettings(): Promise<Settings> {
  const response = await apiFetch("/api/settings");
  if (!response.ok) {
    throw new Error(`Failed to get settings: ${response.statusText}`);
  }
  return await response.json();
}

/**
 * Update settings in the Rust backend (partial PATCH).
 */
export async function updateSettings(settings: Partial<Settings>): Promise<Settings> {
  const response = await apiFetch("/api/settings", {
    method: "PATCH",
    body: JSON.stringify(settings),
  });
  if (!response.ok) {
    throw new Error(`Failed to update settings: ${response.statusText}`);
  }
  return await response.json();
}

/**
 * Get API keys from the Rust backend
 */
export async function getApiKeys(): Promise<ApiKey[]> {
  const response = await apiFetch("/api/keys");
  if (!response.ok) {
    throw new Error(`Failed to get API keys: ${response.statusText}`);
  }
  const data = await response.json();
  return data.keys || [];
}

/**
 * Enable tunnel
 */
export async function enableTunnel(): Promise<Record<string, unknown>> {
  const response = await apiFetch("/api/tunnel/enable", { method: "POST" });
  if (!response.ok) {
    throw new Error(`Failed to enable tunnel: ${response.statusText}`);
  }
  return await response.json();
}

/**
 * Disable tunnel
 */
export async function disableTunnel(): Promise<Record<string, unknown>> {
  const response = await apiFetch("/api/tunnel/disable", { method: "POST" });
  if (!response.ok) {
    throw new Error(`Failed to disable tunnel: ${response.statusText}`);
  }
  return await response.json();
}

/**
 * Get tunnel status
 */
export async function getTunnelStatus(): Promise<TunnelStatus> {
  const response = await apiFetch("/api/tunnel/status");
  if (!response.ok) {
    throw new Error(`Failed to get tunnel status: ${response.statusText}`);
  }
  return await response.json();
}

/**
 * Enable Tailscale
 */
export async function enableTailscale(): Promise<Record<string, unknown>> {
  const response = await apiFetch("/api/tunnel/tailscale-enable", {
    method: "POST",
  });
  if (!response.ok) {
    throw new Error(`Failed to enable Tailscale: ${response.statusText}`);
  }
  return await response.json();
}

/**
 * Disable Tailscale
 */
export async function disableTailscale(): Promise<Record<string, unknown>> {
  const response = await apiFetch("/api/tunnel/tailscale-disable", {
    method: "POST",
  });
  if (!response.ok) {
    throw new Error(`Failed to disable Tailscale: ${response.statusText}`);
  }
  return await response.json();
}

/**
 * Get console logs from Rust backend
 */
export async function getConsoleLogs(): Promise<ConsoleLog[]> {
  const response = await apiFetch("/api/observability/logs");
  if (!response.ok) {
    throw new Error(`Failed to get console logs: ${response.statusText}`);
  }
  const data = await response.json();
  return data.logs || [];
}

/**
 * Get MITM config
 */
export async function getMitmConfig(): Promise<MitmConfig> {
  const response = await apiFetch("/api/mitm-config");
  if (!response.ok) {
    throw new Error(`Failed to get MITM config: ${response.statusText}`);
  }
  return await response.json();
}

/**
 * Start MITM proxy
 */
export async function startMitm(): Promise<Record<string, unknown>> {
  const response = await apiFetch("/api/mitm/start", { method: "POST" });
  if (!response.ok) {
    throw new Error(`Failed to start MITM: ${response.statusText}`);
  }
  return await response.json();
}

/**
 * Stop MITM proxy
 */
export async function stopMitm(): Promise<Record<string, unknown>> {
  const response = await apiFetch("/api/mitm/stop", { method: "POST" });
  if (!response.ok) {
    throw new Error(`Failed to stop MITM: ${response.statusText}`);
  }
  return await response.json();
}

/**
 * Check if cloud sync is enabled
 */
export async function isCloudEnabled(): Promise<boolean> {
  const settings = await getSettings();
  return settings.cloudEnabled === true;
}

/**
 * Trigger one quota auto-ping scheduler tick on the Rust backend.
 * Foundation endpoint — full warm-ping may still be partial.
 */
export async function runQuotaAutoPingTick(): Promise<Record<string, unknown>> {
  const response = await apiFetch("/api/quota/auto-ping/tick", { method: "POST" });
  if (!response.ok) {
    throw new Error(`Failed to run auto-ping tick: ${response.statusText}`);
  }
  return await response.json();
}
