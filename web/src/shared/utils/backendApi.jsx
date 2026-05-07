/**
 * Helper functions for calling the Rust backend API from NextJS SSR/client code.
 * These replace the local JS backend modules that were previously used.
 */

const BACKEND_URL = process.env.NEXT_PUBLIC_BASE_URL ?? process.env.BASE_URL ?? "http://127.0.0.1:4623";

/**
 * Get settings from the Rust backend
 */
export async function getSettings() {
  const response = await fetch(`${BACKEND_URL}/api/settings`);
  if (!response.ok) {
    throw new Error(`Failed to get settings: ${response.statusText}`);
  }
  return await response.json();
}

/**
 * Update settings in the Rust backend
 */
export async function updateSettings(settings) {
  const response = await fetch(`${BACKEND_URL}/api/settings`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
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
export async function getApiKeys() {
  const response = await fetch(`${BACKEND_URL}/api/keys`);
  if (!response.ok) {
    throw new Error(`Failed to get API keys: ${response.statusText}`);
  }
  const data = await response.json();
  return data.keys || [];
}

/**
 * Enable tunnel
 */
export async function enableTunnel() {
  const response = await fetch(`${BACKEND_URL}/api/tunnel/enable`, {
    method: 'POST',
  });
  if (!response.ok) {
    throw new Error(`Failed to enable tunnel: ${response.statusText}`);
  }
  return await response.json();
}

/**
 * Disable tunnel
 */
export async function disableTunnel() {
  const response = await fetch(`${BACKEND_URL}/api/tunnel/disable`, {
    method: 'POST',
  });
  if (!response.ok) {
    throw new Error(`Failed to disable tunnel: ${response.statusText}`);
  }
  return await response.json();
}

/**
 * Get tunnel status
 */
export async function getTunnelStatus() {
  const response = await fetch(`${BACKEND_URL}/api/tunnel/status`);
  if (!response.ok) {
    throw new Error(`Failed to get tunnel status: ${response.statusText}`);
  }
  return await response.json();
}

/**
 * Enable Tailscale
 */
export async function enableTailscale() {
  const response = await fetch(`${BACKEND_URL}/api/tunnel/tailscale-enable`, {
    method: 'POST',
  });
  if (!response.ok) {
    throw new Error(`Failed to enable Tailscale: ${response.statusText}`);
  }
  return await response.json();
}

/**
 * Disable Tailscale
 */
export async function disableTailscale() {
  const response = await fetch(`${BACKEND_URL}/api/tunnel/tailscale-disable`, {
    method: 'POST',
  });
  if (!response.ok) {
    throw new Error(`Failed to disable Tailscale: ${response.statusText}`);
  }
  return await response.json();
}

/**
 * Get console logs from Rust backend
 */
export async function getConsoleLogs() {
  const response = await fetch(`${BACKEND_URL}/api/observability/logs`);
  if (!response.ok) {
    throw new Error(`Failed to get console logs: ${response.statusText}`);
  }
  const data = await response.json();
  return data.logs || [];
}

/**
 * Get MITM config
 */
export async function getMitmConfig() {
  const response = await fetch(`${BACKEND_URL}/api/mitm-config`);
  if (!response.ok) {
    throw new Error(`Failed to get MITM config: ${response.statusText}`);
  }
  return await response.json();
}

/**
 * Start MITM proxy
 */
export async function startMitm() {
  const response = await fetch(`${BACKEND_URL}/api/mitm/start`, {
    method: 'POST',
  });
  if (!response.ok) {
    throw new Error(`Failed to start MITM: ${response.statusText}`);
  }
  return await response.json();
}

/**
 * Stop MITM proxy
 */
export async function stopMitm() {
  const response = await fetch(`${BACKEND_URL}/api/mitm/stop`, {
    method: 'POST',
  });
  if (!response.ok) {
    throw new Error(`Failed to stop MITM: ${response.statusText}`);
  }
  return await response.json();
}

/**
 * Check if cloud sync is enabled
 */
export async function isCloudEnabled() {
  const settings = await getSettings();
  return settings.cloudEnabled === true;
}
