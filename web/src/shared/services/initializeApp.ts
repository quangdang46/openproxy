import {
  getSettings, getApiKeys,
  enableTunnel, getTunnelStatus,
  enableTailscale,
  getMitmConfig, startMitm,
} from "@/shared/utils/backendApi";

process.setMaxListeners(20);

interface AppSingleton {
  initialized: boolean;
  mitmStartInProgress: boolean;
}

// Survive Next.js hot reload
const g: AppSingleton = (global as any).__appSingleton ??= {
  initialized: false,
  mitmStartInProgress: false,
};

export async function initializeApp(): Promise<void> {
  if (g.initialized) return;

  try {
    const settings = await getSettings();

    // Auto-resume tunnel
    if (settings.tunnelEnabled) {
      console.log("[InitApp] Tunnel was enabled, auto-resuming...");
      safeRestartTunnel("startup").catch((e) => console.log("[InitApp] Tunnel resume failed:", (e as Error).message));
    }

    // Auto-resume tailscale
    if (settings.tailscaleEnabled) {
      console.log("[InitApp] Tailscale was enabled, auto-resuming...");
      safeRestartTailscale("startup").catch((e) => console.log("[InitApp] Tailscale resume failed:", (e as Error).message));
    }

    autoStartMitm();

    g.initialized = true;
  } catch (error) {
    console.error("[InitApp] Error:", error);
  }
}

async function autoStartMitm(): Promise<void> {
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
    console.log("[InitApp] MITM auto-start failed:", (err as Error).message);
  } finally {
    g.mitmStartInProgress = false;
  }
}

async function safeRestartTunnel(reason: string): Promise<void> {
  const settings = await getSettings();
  if (!settings.tunnelEnabled) return;

  const tunnelStatus = await getTunnelStatus();
  if (tunnelStatus.tunnel?.running) return;

  console.log(`[Tunnel] safeRestart (${reason})`);
  try {
    await enableTunnel();
    console.log("[Tunnel] restart success");
  } catch (err) {
    console.log("[Tunnel] restart failed:", (err as Error).message);
  }
}

async function safeRestartTailscale(reason: string): Promise<void> {
  const settings = await getSettings();
  if (!settings.tailscaleEnabled) return;

  const tunnelStatus = await getTunnelStatus();
  if (tunnelStatus.tailscale?.running) return;

  console.log(`[Tailscale] safeRestart (${reason})`);
  try {
    await enableTailscale();
    console.log("[Tailscale] restart success");
  } catch (err) {
    console.log("[Tailscale] restart failed:", (err as Error).message);
  }
}

export default initializeApp;
