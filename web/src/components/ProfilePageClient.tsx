"use client";

import { useState, useEffect, useRef, useCallback } from "react";
import { Card, Button, Toggle, Input, LanguageSwitcher } from "@/shared/components";
import { ConfirmModal } from "@/shared/components/Modal";
import { useTheme } from "@/shared/hooks/useTheme";
import { cn } from "@/shared/utils/cn";
import { APP_CONFIG } from "@/shared/constants/config";

// ── Types ────────────────────────────────────────────────────────────

interface Settings {
  requireLogin?: boolean;
  hasPassword?: boolean;
  observabilityEnabled?: boolean;
  outboundProxyEnabled?: boolean;
  outboundProxyUrl?: string;
  outboundNoProxy?: string;
  comboStrategy?: string;
  stickyRoundRobinLimit?: number;
  [key: string]: unknown;
}

interface StatusMessage {
  type: "success" | "error" | "info";
  message: string;
}

// ── Helpers ────────────────────────────────────────────────────────────

function StatusAlert({ status }: { status: StatusMessage | null }) {
  if (!status) return null;
  const cls =
    status.type === "success"
      ? "text-green-600 dark:text-green-400"
      : status.type === "error"
        ? "text-red-500"
        : "text-blue-600 dark:text-blue-400";
  return <p className={`text-xs sm:text-sm ${cls}`}>{status.message}</p>;
}

// ── Component ────────────────────────────────────────────────────────

export default function ProfilePageClient() {
  const { theme, setTheme } = useTheme();
  const [langOpen, setLangOpen] = useState(false);
  const [shutdownOpen, setShutdownOpen] = useState(false);
  const [isShuttingDown, setIsShuttingDown] = useState(false);
  const [settings, setSettings] = useState<Settings>({});
  const [loading, setLoading] = useState(true);

  // Password fields
  const [passwords, setPasswords] = useState({ current: "", newPass: "", confirm: "" });
  const [passStatus, setPassStatus] = useState<StatusMessage | null>(null);
  const [passLoading, setPassLoading] = useState(false);

  // Database
  const [dbLoading, setDbLoading] = useState(false);
  const [dbStatus, setDbStatus] = useState<StatusMessage | null>(null);

  // Outbound proxy
  const [proxyForm, setProxyForm] = useState({
    outboundProxyUrl: "",
    outboundNoProxy: "",
  });
  const [proxyStatus, setProxyStatus] = useState<StatusMessage | null>(null);
  const [proxyLoading, setProxyLoading] = useState(false);
  const [proxyTestLoading, setProxyTestLoading] = useState(false);

  // Combo sticky limit input
  const [comboStickyLimitInput, setComboStickyLimitInput] = useState("1");

  const importFileRef = useRef<HTMLInputElement>(null);

  // ── Load settings on mount ──────────────────────────────────────────

  const fetchSettings = useCallback(async () => {
    try {
      const res = await fetch("/api/settings");
      if (!res.ok) throw new Error(`Server returned ${res.status}`);
      const data = (await res.json()) as Settings;
      setSettings(data);
      setProxyForm({
        outboundProxyUrl: data.outboundProxyUrl ?? "",
        outboundNoProxy: data.outboundNoProxy ?? "",
      });
      setComboStickyLimitInput(String(data.stickyRoundRobinLimit ?? 1));
    } catch (err) {
      console.error("Failed to fetch settings:", err);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void fetchSettings();
  }, [fetchSettings]);

  // ── Generic settings patch ──────────────────────────────────────────

  const patchSettings = useCallback(
    async (payload: Record<string, unknown>): Promise<Settings | null> => {
      try {
        const res = await fetch("/api/settings", {
          method: "PATCH",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(payload),
        });
        const data = (await res.json()) as Settings;
        if (!res.ok) {
          const errMsg = (data as unknown as { error?: string }).error ?? "Request failed";
          throw new Error(errMsg);
        }
        return data;
      } catch (err) {
        throw err;
      }
    },
    [],
  );

  // ── Require Login toggle ────────────────────────────────────────────

  const updateRequireLogin = async (value: boolean) => {
    try {
      const data = await patchSettings({ requireLogin: value });
      if (data) setSettings((prev) => ({ ...prev, ...data }));
    } catch (err) {
      console.error("Failed to update requireLogin:", err);
    }
  };

  // ── Password management (UI only — OpenProxy needs a dedicated endpoint) ─

  const handlePasswordChange = async (e: React.FormEvent) => {
    e.preventDefault();
    if (passwords.newPass !== passwords.confirm) {
      setPassStatus({ type: "error", message: "Passwords do not match" });
      return;
    }

    setPassLoading(true);
    setPassStatus(null);

    try {
      const data = await patchSettings({
        currentPassword: passwords.current,
        newPassword: passwords.newPass,
      });
      if (data) {
        setPassStatus({ type: "success", message: "Password updated successfully" });
        setPasswords({ current: "", newPass: "", confirm: "" });
        setSettings((prev) => ({ ...prev, ...data }));
      }
    } catch (err) {
      const msg = err instanceof Error ? err.message : "An error occurred";
      // OpenProxy returns NOT_IMPLEMENTED for password changes via /api/settings
      if (msg.includes("NOT_IMPLEMENTED") || msg.includes("dedicated endpoint")) {
        setPassStatus({
          type: "info",
          message: "Password management via the dashboard is not yet available. Use the `openproxy auth set-password` CLI command.",
        });
      } else {
        setPassStatus({ type: "error", message: msg });
      }
    } finally {
      setPassLoading(false);
    }
  };

  // ── Observability toggle ────────────────────────────────────────────

  const updateObservability = async (enabled: boolean) => {
    try {
      const data = await patchSettings({ observabilityEnabled: enabled });
      if (data) setSettings((prev) => ({ ...prev, ...data }));
    } catch (err) {
      console.error("Failed to update observability:", err);
    }
  };

  // ── Combo strategy toggle ───────────────────────────────────────────

  const updateComboStrategy = async (strategy: string) => {
    try {
      const data = await patchSettings({ comboStrategy: strategy });
      if (data) setSettings((prev) => ({ ...prev, ...data }));
    } catch (err) {
      console.error("Failed to update combo strategy:", err);
    }
  };

  // ── Sticky round-robin limit ────────────────────────────────────────

  const updateStickyLimit = async (raw: string) => {
    const num = parseInt(raw, 10);
    if (isNaN(num) || num < 1) return;
    try {
      const data = await patchSettings({ stickyRoundRobinLimit: num });
      if (data) {
        setSettings((prev) => ({ ...prev, ...data }));
        setComboStickyLimitInput(String(num));
      }
    } catch (err) {
      console.error("Failed to update sticky limit:", err);
    }
  };

  // ── Outbound proxy ──────────────────────────────────────────────────

  const updateProxyEnabled = async (enabled: boolean) => {
    setProxyLoading(true);
    setProxyStatus(null);
    try {
      const data = await patchSettings({ outboundProxyEnabled: enabled });
      if (data) setSettings((prev) => ({ ...prev, ...data }));
      setProxyStatus({ type: "success", message: enabled ? "Proxy enabled" : "Proxy disabled" });
    } catch (err) {
      const msg = err instanceof Error ? err.message : "An error occurred";
      setProxyStatus({ type: "error", message: msg });
    } finally {
      setProxyLoading(false);
    }
  };

  const updateProxyConfig = async (e: React.FormEvent) => {
    e.preventDefault();
    if (settings.outboundProxyEnabled !== true) return;
    setProxyLoading(true);
    setProxyStatus(null);
    try {
      const data = await patchSettings({
        outboundProxyUrl: proxyForm.outboundProxyUrl,
        outboundNoProxy: proxyForm.outboundNoProxy,
      });
      if (data) setSettings((prev) => ({ ...prev, ...data }));
      setProxyStatus({ type: "success", message: "Proxy settings applied" });
    } catch (err) {
      const msg = err instanceof Error ? err.message : "An error occurred";
      setProxyStatus({ type: "error", message: msg });
    } finally {
      setProxyLoading(false);
    }
  };

  const testOutboundProxy = async () => {
    const proxyUrl = proxyForm.outboundProxyUrl.trim();
    if (!proxyUrl) {
      setProxyStatus({ type: "error", message: "Please enter a Proxy URL to test" });
      return;
    }
    setProxyTestLoading(true);
    setProxyStatus(null);
    try {
      const res = await fetch("/api/settings/proxy-test", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ proxyUrl }),
      });
      const data = await res.json().catch(() => ({}));
      if (res.ok && (data as { ok?: boolean }).ok === true) {
        setProxyStatus({
          type: "success",
          message: `Proxy test OK (${(data as { status?: string }).status ?? "connected"}) in ${(data as { elapsedMs?: number }).elapsedMs ?? "?"}ms`,
        });
      } else {
        setProxyStatus({
          type: "error",
          message: (data as { error?: string }).error ?? "Proxy test failed",
        });
      }
    } catch {
      setProxyStatus({ type: "error", message: "Proxy test request failed" });
    } finally {
      setProxyTestLoading(false);
    }
  };

  // ── Database export / import ────────────────────────────────────────

  const handleExportDatabase = async () => {
    setDbLoading(true);
    setDbStatus(null);
    try {
      const res = await fetch("/api/settings/database");
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error((data as { error?: string }).error ?? "Failed to export database");
      }
      const payload = await res.json();
      const content = JSON.stringify(payload, null, 2);
      const blob = new Blob([content], { type: "application/json" });
      const url = URL.createObjectURL(blob);
      const anchor = document.createElement("a");
      const stamp = new Date().toISOString().replace(/[.:]/g, "-");
      anchor.href = url;
      anchor.download = `openproxy-backup-${stamp}.json`;
      document.body.appendChild(anchor);
      anchor.click();
      document.body.removeChild(anchor);
      URL.revokeObjectURL(url);
      setDbStatus({ type: "success", message: "Database backup downloaded" });
    } catch (err) {
      setDbStatus({ type: "error", message: err instanceof Error ? err.message : "Failed to export database" });
    } finally {
      setDbLoading(false);
    }
  };

  const handleImportDatabase = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) return;
    setDbLoading(true);
    setDbStatus(null);
    try {
      const text = await file.text();
      // Validate JSON before sending
      JSON.parse(text); // throws if invalid
      const res = await fetch("/api/settings/database", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: text,
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error((data as { error?: string }).error ?? "Failed to import database");
      }
      await fetchSettings();
      setDbStatus({ type: "success", message: `Database imported from ${file.name}` });
    } catch (err) {
      setDbStatus({ type: "error", message: err instanceof Error ? err.message : "Invalid backup file" });
    } finally {
      setDbLoading(false);
    }
  };

  // ── Shutdown ────────────────────────────────────────────────────────

  const handleShutdown = async () => {
    setIsShuttingDown(true);
    try {
      await fetch("/api/shutdown", { method: "POST" });
    } catch {
      // Expected to fail as server shuts down
    }
    setIsShuttingDown(false);
    setShutdownOpen(false);
  };

  // ── Logout ──────────────────────────────────────────────────────────

  const handleLogout = async () => {
    try {
      const res = await fetch("/api/auth/logout", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({}),
      });
      if (res.ok) {
        window.location.href = "/login";
      }
    } catch (err) {
      console.error("Failed to logout:", err);
    }
  };

  // ── Derived state ───────────────────────────────────────────────────

  const requireLogin = settings.requireLogin === true;
  const hasPassword = settings.hasPassword === true;
  const observabilityEnabled = settings.observabilityEnabled === true;
  const outboundProxyEnabled = settings.outboundProxyEnabled === true;
  const comboRoundRobin = settings.comboStrategy === "round-robin";
  const stickyLimit = settings.stickyRoundRobinLimit ?? 1;

  // ── Render ──────────────────────────────────────────────────────────

  return (
    <div className="max-w-2xl mx-auto">
      <div className="flex flex-col gap-6">
        {/* ── Theme / Local Mode Info ─────────────────────────────── */}
        <Card>
          <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-4 mb-4">
            <div className="flex items-center gap-3 sm:gap-4">
              <div className="size-10 sm:size-12 rounded-lg bg-green-500/10 text-green-500 flex items-center justify-center shrink-0">
                <span className="material-symbols-outlined text-xl sm:text-2xl">computer</span>
              </div>
              <div>
                <h2 className="text-lg sm:text-xl font-semibold">Appearance</h2>
                <p className="text-sm text-muted-soft">Theme preference</p>
              </div>
            </div>
            <div className="inline-flex p-1 rounded-lg bg-surface-2 w-full sm:w-auto">
              {(["light", "dark", "system"] as const).map((option) => (
                <button
                  key={option}
                  type="button"
                  onClick={() => setTheme(option)}
                  className={cn(
                    "flex items-center justify-center gap-1 sm:gap-1.5 px-2 sm:px-3 py-1.5 rounded-md font-medium transition-all flex-1 sm:flex-initial text-[13px]",
                    theme === option
                      ? "bg-canvas text-ink shadow-sm"
                      : "text-muted-soft hover:text-ink",
                  )}
                >
                  <span className="material-symbols-outlined text-[18px]">
                    {option === "light" ? "light_mode" : option === "dark" ? "dark_mode" : "contrast"}
                  </span>
                  <span className="capitalize text-xs sm:text-sm">{option}</span>
                </button>
              ))}
            </div>
          </div>

          <div className="flex flex-col gap-3 pt-4 border-t border-hairline-soft">
            <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between p-3 rounded-lg bg-surface-card border border-hairline gap-2">
              <div>
                <p className="font-medium text-sm">Database Location</p>
                <p className="text-xs sm:text-sm text-muted-soft font-mono break-all">
                  {APP_CONFIG.name.toLowerCase()} internal storage
                </p>
              </div>
            </div>
          </div>
        </Card>

        {/* ── Language ────────────────────────────────────────────── */}
        <Card>
          <div className="flex items-center gap-3 mb-4">
            <div className="size-10 rounded-lg bg-blue-500/10 text-blue-500 flex items-center justify-center shrink-0">
              <span className="material-symbols-outlined text-[20px]">language</span>
            </div>
            <h3 className="text-base sm:text-lg font-semibold">Language</h3>
          </div>
          <button
            onClick={() => setLangOpen(true)}
            className="flex items-center justify-between w-full p-3 rounded-lg bg-surface-card border border-hairline hover:border-ink/40 transition-colors"
            data-i18n-skip="true"
          >
            <span className="text-sm text-muted-soft">Display language</span>
            <span className="material-symbols-outlined text-muted-soft">chevron_right</span>
          </button>
        </Card>

        {/* ── Security: requireLogin + password ───────────────────── */}
        <Card>
          <div className="flex items-center gap-3 mb-4">
            <div className="size-10 rounded-lg bg-amber-500/10 text-amber-500 flex items-center justify-center shrink-0">
              <span className="material-symbols-outlined text-[20px]">shield</span>
            </div>
            <h3 className="text-base sm:text-lg font-semibold">Security</h3>
          </div>
          <div className="flex flex-col gap-4">
            <div className="flex items-start sm:items-center justify-between gap-4">
              <div className="flex-1 min-w-0">
                <p className="font-medium text-sm">Require login</p>
                <p className="text-xs text-muted-soft">
                  When ON, the dashboard requires a password to access. When OFF, access is unrestricted.
                </p>
              </div>
              <Toggle
                checked={requireLogin}
                onChange={() => updateRequireLogin(!requireLogin)}
                disabled={loading}
              />
            </div>

            {requireLogin && (
              <form onSubmit={handlePasswordChange} className="flex flex-col gap-4 pt-4 border-t border-hairline-soft">
                {hasPassword && (
                  <div className="flex flex-col gap-2">
                    <label className="text-xs sm:text-sm font-medium">Current Password</label>
                    <Input
                      type="password"
                      placeholder="Enter current password"
                      value={passwords.current}
                      onChange={(e) => setPasswords((p) => ({ ...p, current: e.target.value }))}
                      required
                    />
                  </div>
                )}

                <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
                  <div className="flex flex-col gap-2">
                    <label className="text-xs sm:text-sm font-medium">
                      {hasPassword ? "New Password" : "Set Password"}
                    </label>
                    <Input
                      type="password"
                      placeholder={hasPassword ? "Enter new password" : "Choose a password"}
                      value={passwords.newPass}
                      onChange={(e) => setPasswords((p) => ({ ...p, newPass: e.target.value }))}
                      required
                    />
                  </div>
                  <div className="flex flex-col gap-2">
                    <label className="text-xs sm:text-sm font-medium">Confirm Password</label>
                    <Input
                      type="password"
                      placeholder="Confirm password"
                      value={passwords.confirm}
                      onChange={(e) => setPasswords((p) => ({ ...p, confirm: e.target.value }))}
                      required
                    />
                  </div>
                </div>

                <StatusAlert status={passStatus} />

                <div className="pt-2">
                  <Button type="submit" variant="primary" loading={passLoading} className="w-full sm:w-auto">
                    {hasPassword ? "Update Password" : "Set Password"}
                  </Button>
                </div>
              </form>
            )}
          </div>
        </Card>

        {/* ── Routing Preferences ─────────────────────────────────── */}
        <Card>
          <div className="flex items-center gap-3 mb-4">
            <div className="size-10 rounded-lg bg-blue-500/10 text-blue-500 flex items-center justify-center shrink-0">
              <span className="material-symbols-outlined text-[20px]">route</span>
            </div>
            <h3 className="text-base sm:text-lg font-semibold">Routing Strategy</h3>
          </div>
          <div className="flex flex-col gap-4">
            {/* Combo Round Robin */}
            <div className="flex items-start sm:items-center justify-between gap-4">
              <div className="flex-1 min-w-0">
                <p className="font-medium text-sm">Combo Round Robin</p>
                <p className="text-xs text-muted-soft">
                  Cycle through providers in combos instead of always starting with the first
                </p>
              </div>
              <Toggle
                checked={comboRoundRobin}
                onChange={() => updateComboStrategy(comboRoundRobin ? "fallback" : "round-robin")}
                disabled={loading}
              />
            </div>

            {/* Sticky Round Robin Limit */}
            <div className="flex items-start sm:items-center justify-between gap-4 pt-2 border-t border-hairline-soft">
              <div className="flex-1 min-w-0">
                <p className="font-medium text-sm">Sticky Limit</p>
                <p className="text-xs text-muted-soft">
                  Provider requests per account before switching
                </p>
              </div>
              <Input
                type="number"
                min="1"
                max="100"
                value={comboStickyLimitInput}
                onChange={(e) => setComboStickyLimitInput(e.target.value)}
                onBlur={() => updateStickyLimit(comboStickyLimitInput)}
                disabled={loading}
                className="w-16 sm:w-20 text-center shrink-0"
              />
            </div>

            <p className="text-xs text-muted-soft italic pt-2 border-t border-hairline-soft">
              {comboRoundRobin
                ? `Combos rotate across providers, with up to ${stickyLimit} call${stickyLimit === 1 ? "" : "s"} per account before switching.`
                : "Combos always start with their first model (fallback strategy)."}
            </p>
          </div>
        </Card>

        {/* ── Network / Outbound Proxy ────────────────────────────── */}
        <Card>
          <div className="flex items-center gap-3 mb-4">
            <div className="size-10 rounded-lg bg-purple-500/10 text-purple-500 flex items-center justify-center shrink-0">
              <span className="material-symbols-outlined text-[20px]">wifi</span>
            </div>
            <h3 className="text-base sm:text-lg font-semibold">Network</h3>
          </div>

          <div className="flex flex-col gap-4">
            <div className="flex items-start sm:items-center justify-between gap-4">
              <div className="flex-1 min-w-0">
                <p className="font-medium text-sm">Outbound Proxy</p>
                <p className="text-xs text-muted-soft">
                  Enable proxy for OAuth and provider outbound requests
                </p>
              </div>
              <Toggle
                checked={outboundProxyEnabled}
                onChange={() => updateProxyEnabled(!outboundProxyEnabled)}
                disabled={loading || proxyLoading}
              />
            </div>

            {outboundProxyEnabled && (
              <form onSubmit={updateProxyConfig} className="flex flex-col gap-4 pt-2 border-t border-hairline-soft">
                <div className="flex flex-col gap-2">
                  <label className="font-medium text-sm">Proxy URL</label>
                  <Input
                    placeholder="http://127.0.0.1:7897"
                    value={proxyForm.outboundProxyUrl}
                    onChange={(e) => setProxyForm((p) => ({ ...p, outboundProxyUrl: e.target.value }))}
                    disabled={loading || proxyLoading}
                  />
                  <p className="text-xs text-muted-soft">
                    Leave empty to inherit the existing environment proxy (if any).
                  </p>
                </div>

                <div className="flex flex-col gap-2 pt-2 border-t border-hairline-soft">
                  <label className="font-medium text-sm">No Proxy</label>
                  <Input
                    placeholder="localhost,127.0.0.1"
                    value={proxyForm.outboundNoProxy}
                    onChange={(e) => setProxyForm((p) => ({ ...p, outboundNoProxy: e.target.value }))}
                    disabled={loading || proxyLoading}
                  />
                  <p className="text-xs text-muted-soft">
                    Comma-separated hostnames or domains to bypass the proxy.
                  </p>
                </div>

                <div className="pt-2 border-t border-hairline-soft flex flex-col sm:flex-row items-stretch sm:items-center gap-2">
                  <Button
                    type="button"
                    variant="secondary"
                    loading={proxyTestLoading}
                    disabled={loading || proxyLoading}
                    onClick={testOutboundProxy}
                    className="w-full sm:w-auto"
                  >
                    Test proxy URL
                  </Button>
                  <Button type="submit" variant="primary" loading={proxyLoading} className="w-full sm:w-auto">
                    Apply
                  </Button>
                </div>
              </form>
            )}

            <StatusAlert status={proxyStatus} />
          </div>
        </Card>

        {/* ── Observability ────────────────────────────────────────── */}
        <Card>
          <div className="flex items-center gap-3 mb-4">
            <div className="size-10 rounded-lg bg-orange-500/10 text-orange-500 flex items-center justify-center shrink-0">
              <span className="material-symbols-outlined text-[20px]">monitoring</span>
            </div>
            <h3 className="text-base sm:text-lg font-semibold">Observability</h3>
          </div>
          <div className="flex items-start sm:items-center justify-between gap-4">
            <div className="flex-1 min-w-0">
              <p className="font-medium text-sm">Enable Request Logging</p>
              <p className="text-xs text-muted-soft">
                Record request details for inspection in the console log view
              </p>
            </div>
            <Toggle
              checked={observabilityEnabled}
              onChange={updateObservability}
              disabled={loading}
            />
          </div>
        </Card>

        {/* ── Database Management ──────────────────────────────────── */}
        <Card>
          <div className="flex items-center gap-3 mb-4">
            <div className="size-10 rounded-lg bg-teal-500/10 text-teal-500 flex items-center justify-center shrink-0">
              <span className="material-symbols-outlined text-[20px]">database</span>
            </div>
            <h3 className="text-base sm:text-lg font-semibold">Database</h3>
          </div>
          <div className="flex flex-col gap-4">
            <p className="text-xs sm:text-sm text-muted-soft">
              Export your entire database as JSON, or restore from a previous backup file.
            </p>

            <div className="flex flex-col sm:flex-row gap-2">
              <Button
                variant="secondary"
                icon="download"
                onClick={handleExportDatabase}
                loading={dbLoading}
                className="w-full sm:w-auto"
              >
                Download Backup
              </Button>
              <Button
                variant="outline"
                icon="upload"
                onClick={() => importFileRef.current?.click()}
                disabled={dbLoading}
                className="w-full sm:w-auto"
              >
                Import Backup
              </Button>
              <input
                ref={importFileRef}
                type="file"
                accept="application/json,.json"
                className="hidden"
                onChange={handleImportDatabase}
              />
            </div>

            <StatusAlert status={dbStatus} />
          </div>
        </Card>

        {/* ── Account actions ─────────────────────────────────────── */}
        <div className="flex flex-col sm:flex-row gap-2">
          <Button
            variant="outline"
            fullWidth
            icon="power_settings_new"
            onClick={() => setShutdownOpen(true)}
            className="text-[color:var(--color-danger)] border-[color:var(--color-danger)]/40 hover:bg-[color:var(--color-danger)]/10 hover:border-[color:var(--color-danger)]/60"
          >
            Shutdown
          </Button>
          <Button variant="outline" fullWidth icon="logout" onClick={handleLogout}>
            Logout
          </Button>
        </div>

        {/* ── App Info ────────────────────────────────────────────── */}
        <div className="text-center text-xs sm:text-sm text-muted-soft py-4">
          <p>
            {APP_CONFIG.name} v{APP_CONFIG.version}
          </p>
          <p className="mt-1">Local Mode &mdash; All data stored on your machine</p>
        </div>
      </div>

      {/* ── Language Switcher Modal ───────────────────────────────── */}
      <LanguageSwitcher
        hideTrigger
        isOpen={langOpen}
        onClose={() => setLangOpen(false)}
      />

      {/* ── Shutdown Confirm Modal ────────────────────────────────── */}
      <ConfirmModal
        isOpen={shutdownOpen}
        onClose={() => setShutdownOpen(false)}
        onConfirm={handleShutdown}
        title="Shutdown Server"
        message="Are you sure you want to stop the proxy server?"
        confirmText="Shutdown"
        cancelText="Cancel"
        variant="danger"
        loading={isShuttingDown}
      />
    </div>
  );
}
