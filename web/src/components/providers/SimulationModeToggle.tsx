"use client";

import { useState, useEffect, useCallback } from "react";

import { useNotificationStore } from "@/store/notificationStore";

// ── Simulation mode toggle (bead sim-20) ───────────────────────
// Compact Real/Mock segmented control + effective-mode banner.
// Reads GET /api/mock/status, writes PUT /api/providers/<id> {mode}.
// <id> may be a connection id OR a provider name (mode-only writes need no
// connection row — the point of mock mode is testing without credentials).

interface SimMode {
  configured: string;
  effective: string;
  reason: string;
  simulationSupported: boolean;
}

interface SimulationModeToggleProps {
  providerId: string;
  connectionId?: string | null;
  onChanged?: () => void;
}

export function useSimulationMode(providerId: string) {
  const [mode, setMode] = useState<SimMode | null>(null);
  const [loading, setLoading] = useState(true);

  const refresh = useCallback(async () => {
    try {
      const res = await fetch("/api/mock/status", { cache: "no-store" });
      if (!res.ok) return;
      const data = await res.json();
      const entry = data?.providers?.[providerId];
      if (entry) {
        setMode({
          configured: entry.configured ?? "real",
          effective: entry.effective ?? "real",
          reason: entry.reason ?? "default",
          simulationSupported: entry.simulationSupported ?? false,
        });
      }
    } finally {
      setLoading(false);
    }
  }, [providerId]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  return { mode, loading, refresh };
}

export default function SimulationModeToggle({ providerId, connectionId, onChanged }: SimulationModeToggleProps) {
  const { mode, loading, refresh } = useSimulationMode(providerId);
  const [saving, setSaving] = useState(false);
  const notify = useNotificationStore();

  const setProviderMode = async (next: "real" | "mock") => {
    setSaving(true);
    try {
      const target = connectionId || providerId;
      const res = await fetch(`/api/providers/${target}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ mode: next }),
      });
      if (!res.ok) {
        const err = await res.json().catch(() => ({}));
        notify.error(err.error || `Failed to set mode (${res.status})`);
        return;
      }
      await refresh();
      onChanged?.();
      notify.success(`Simulation mode: ${next}`);
    } finally {
      setSaving(false);
    }
  };

  if (loading || !mode) return null;
  if (!mode.simulationSupported) return null;

  const showBanner = mode.configured !== mode.effective;

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center gap-2">
        <span className="text-xs text-text-muted" title="Mock executes locally with no API key; Real forwards to the provider">
          Simulation
        </span>
        <div className="inline-flex rounded-md border border-border overflow-hidden" role="group" aria-label="Simulation mode">
          {(["real", "mock"] as const).map((m) => (
            <button
              key={m}
              type="button"
              disabled={saving}
              onClick={() => setProviderMode(m)}
              className={`px-2.5 py-1 text-xs font-medium transition-colors ${
                mode.configured === m
                  ? m === "mock"
                    ? "bg-purple-500/20 text-purple-600 dark:text-purple-400"
                    : "bg-primary/10 text-primary"
                  : "bg-surface text-text-muted hover:text-primary"
              }`}
              title={m === "mock" ? "Execute locally, no API key needed" : "Forward to the real provider"}
            >
              {m === "mock" ? "🧪 Mock" : "Real"}
            </button>
          ))}
        </div>
      </div>
      {showBanner && (
        <p className="text-xs text-yellow-600 dark:text-yellow-400">
          Effective: {mode.effective.toUpperCase()} via {mode.reason}
          {mode.reason === "OPENPROXY_DEV_MOCK" ? " (env)" : ""}
          {mode.reason === "settings-force" ? " (settings)" : ""}
        </p>
      )}
    </div>
  );
}
