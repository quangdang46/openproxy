import {
  getSettings, getApiKeys,
  enableTunnel, getTunnelStatus,
  enableTailscale,
  getMitmConfig, startMitm,
} from "@/shared/utils/backendApi";

process.setMaxListeners(20);

// Survive Next.js hot reload
const g = global.__appSingleton ??= {
  initialized: false,
  mitmStartInProgress: false,
};

export async function initializeApp() {
  if (g.initialized) return;

  try {
    const settings = await getSettings();

    // Auto-resume tunnel
    if (settings.tunnelEnabled) {
      console.log("[InitApp] Tunnel was enabled, auto-resuming...");
      safeRestartTunnel("startup").catch((e) => console.log("[InitApp] Tunnel resume failed:", e.message));
    }

    // Auto-resume tailscale
    if (settings.tailscaleEnabled) {
      console.log("[InitApp] Tailscale was enabled, auto-resuming...");
      safeRestartTailscale("startup").catch((e) => console.log("[InitApp] Tailscale resume failed:", e.message));
    }

    autoStartMitm();

    g.initialized = true;
  } catch (error) {
    console.error("[InitApp] Error:", error);
  }
}

async function autoStartMitm() {
  if (g.mitmStartInProgress) return;
  g.mitmStartInProgress = true;
  try {
    const settings = await getSettings();
    if (!settings.mitmEnabled) return;

    const mitmConfig = await getMitmConfig();
    if (mitmConfig.enabled) return;

    const keys = await getApiKeys();
    const activeKey = keys.find(k => k.isActive !== false);

    console.log("[InitApp] MITM was enabled, auto-starting...");
    await startMitm();
    console.log("[InitApp] MITM auto-started");
  } catch (err) {
    console.log("[InitApp] MITM auto-start failed:", err.message);
  } finally {
    g.mitmStartInProgress = false;
  }
}

async function safeRestartTunnel(reason) {
  const settings = await getSettings();
  if (!settings.tunnelEnabled) return;

  const tunnelStatus = await getTunnelStatus();
  if (tunnelStatus.tunnel?.running) return;

  console.log(`[Tunnel] safeRestart (${reason})`);
  try {
    await enableTunnel();
    console.log("[Tunnel] restart success");
  } catch (err) {
    console.log("[Tunnel] restart failed:", err.message);
  }
}

async function safeRestartTailscale(reason) {
  const settings = await getSettings();
  if (!settings.tailscaleEnabled) return;

  const tunnelStatus = await getTunnelStatus();
  if (tunnelStatus.tailscale?.running) return;

  console.log(`[Tailscale] safeRestart (${reason})`);
  try {
    await enableTailscale();
    console.log("[Tailscale] restart success");
  } catch (err) {
    console.log("[Tailscale] restart failed:", err.message);
  }
}

export default initializeApp;
