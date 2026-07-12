import {
  getSettings,
  enableTunnel,
  getTunnelStatus,
  enableTailscale,
  getMitmConfig,
  startMitm,
  runQuotaAutoPingTick,
} from "@/shared/utils/backendApi";

/**
 * Browser-side app bootstrap for the Astro + Rust dashboard.
 *
 * 9router ran this as a Next.js server singleton (watchdog, network monitor,
 * quota auto-ping scheduler). OpenProxy owns process supervision in Rust, so
 * this client path only:
 *   1. Resumes tunnel / tailscale / MITM when settings say they should be on
 *   2. Kicks a best-effort quota auto-ping tick while the dashboard is open
 *
 * Full long-running watchdog + auto-ping scheduler belong in the Rust server
 * (see spawn_boot_resume / residual notes). Do not reintroduce Node globals.
 */

const STARTUP_DEFER_MS = 1500;
const QUOTA_AUTOPING_TICK_MS = 60_000;

interface AppSingleton {
  initialized: boolean;
  mitmStartInProgress: boolean;
  quotaTickTimer: ReturnType<typeof setInterval> | null;
}

function getSingleton(): AppSingleton {
  const g = globalThis as typeof globalThis & { __opAppSingleton?: AppSingleton };
  if (!g.__opAppSingleton) {
    g.__opAppSingleton = {
      initialized: false,
      mitmStartInProgress: false,
      quotaTickTimer: null,
    };
  }
  return g.__opAppSingleton;
}

export async function initializeApp(): Promise<void> {
  // SSR / non-browser — nothing to do (Astro may evaluate modules at build).
  if (typeof window === "undefined") return;

  const g = getSingleton();
  if (g.initialized) return;
  g.initialized = true;

  // Defer heavy resume so the first paint / auth cookie settle first.
  window.setTimeout(() => {
    runClientStartup().catch((e) =>
      console.error("[InitApp] deferred startup failed:", (e as Error).message),
    );
  }, STARTUP_DEFER_MS);
}

async function runClientStartup(): Promise<void> {
  try {
    const settings = await getSettings();

    if (settings.tunnelEnabled) {
      console.log("[InitApp] Tunnel was enabled, auto-resuming...");
      safeRestartTunnel("startup").catch((e) =>
        console.log("[InitApp] Tunnel resume failed:", (e as Error).message),
      );
    }

    if (settings.tailscaleEnabled) {
      console.log("[InitApp] Tailscale was enabled, auto-resuming...");
      safeRestartTailscale("startup").catch((e) =>
        console.log("[InitApp] Tailscale resume failed:", (e as Error).message),
      );
    }

    autoStartMitm().catch((e) =>
      console.log("[InitApp] MITM auto-start failed:", (e as Error).message),
    );

    startQuotaAutoPingClient();
  } catch (error) {
    console.error("[InitApp] Error:", error);
  }
}

async function autoStartMitm(): Promise<void> {
  const g = getSingleton();
  if (g.mitmStartInProgress) return;
  g.mitmStartInProgress = true;
  try {
    const mitmConfig = await getMitmConfig();
    // OpenProxy: `enabled` means routes are configured (mitm_alias non-empty).
    // There is no separate settings.mitmEnabled flag; if routes exist, try start
    // (start is idempotent when already running).
    if (!mitmConfig.enabled) return;

    console.log("[InitApp] MITM routes configured, ensuring proxy is running...");
    await startMitm();
    console.log("[InitApp] MITM auto-start requested");
  } catch (err) {
    // MITM start may require local API-key / loopback privileges — best effort.
    console.log("[InitApp] MITM auto-start failed:", (err as Error).message);
  } finally {
    g.mitmStartInProgress = false;
  }
}

async function safeRestartTunnel(reason: string): Promise<void> {
  const settings = await getSettings();
  if (!settings.tunnelEnabled) return;

  const tunnelStatus = await getTunnelStatus();
  const running =
    (tunnelStatus as { tunnel?: { running?: boolean } }).tunnel?.running === true;
  if (running) return;

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
  const running =
    (tunnelStatus as { tailscale?: { running?: boolean } }).tailscale?.running === true;
  if (running) return;

  console.log(`[Tailscale] safeRestart (${reason})`);
  try {
    await enableTailscale();
    console.log("[Tailscale] restart success");
  } catch (err) {
    console.log("[Tailscale] restart failed:", (err as Error).message);
  }
}

/**
 * While the dashboard tab is open, periodically hit the Rust tick endpoint.
 * Full OAuth warm-ping execution lives server-side; this keeps the foundation
 * exercised when the UI is active. Closes with the page (no Node lifetime).
 */
function startQuotaAutoPingClient(): void {
  const g = getSingleton();
  if (g.quotaTickTimer) return;

  const tick = () => {
    runQuotaAutoPingTick().catch((e) =>
      console.log("[AutoPing] client tick failed:", (e as Error).message),
    );
  };

  // Immediate first tick, then interval.
  tick();
  g.quotaTickTimer = setInterval(tick, QUOTA_AUTOPING_TICK_MS);
  console.log("[AutoPing] client tick scheduler started");
}

export default initializeApp;
