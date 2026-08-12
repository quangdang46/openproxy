"use client";

import React, { useCallback, useEffect, useState } from "react";
import { Card } from "@/shared/components";

// 9router PxpipeClient.js REASON_LABELS — reason keys shown in the History table.
const REASON_LABELS: Record<string, string> = {
  applied: "Prompt exceeded threshold",
  below_threshold: "Below size threshold",
  not_profitable: "Compression not profitable",
  below_min_chars: "Below minimum chars",
  below_min_tokens: "Below minimum tokens",
  unsupported_model: "Model not in allowlist",
  unsupported_format: "Non-Claude request format",
  timeout: "Compression timed out",
  transform_error: "Transform error",
  passthrough: "Passthrough",
  disabled: "Disabled",
  not_installed: "Not installed",
};

interface PxpipeStatus {
  installed: boolean;
  installing: boolean;
  version: string | null;
  path: string | null;
  running: boolean;
  loadedAt: string | null;
  uptimeMs: number;
  npmAvailable: boolean;
  mode: string;
  enabled: boolean;
  autoInstall: boolean;
  minChars: number;
  timeoutMs: number;
}

interface PxpipeLogs {
  installLog: string;
  events: unknown[];
}

export default function PxpipePageClient() {
  const [status, setStatus] = useState<PxpipeStatus | null>(null);
  const [stats, setStats] = useState<any>(null);
  const [logs, setLogs] = useState<PxpipeLogs | null>(null);
  const [health, setHealth] = useState<any>(null);
  const [activeTab, setActiveTab] = useState("all");
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [statusRes, statsRes, logsRes] = await Promise.all([
        fetch("/api/pxpipe/status", { headers: { "Cache-Control": "no-store" } }),
        fetch("/api/pxpipe/stats"),
        fetch("/api/pxpipe/logs?limit=50"),
      ]);
      setStatus(await statusRes.json());
      setStats(await statsRes.json());
      setLogs(await logsRes.json());
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const runHealth = useCallback(async () => {
    try {
      const res = await fetch("/api/pxpipe/health", { method: "POST" });
      setHealth(await res.json());
    } catch (e) {
      setHealth({ healthy: false, error: String(e) });
    }
  }, []);

  useEffect(() => {
    refresh();
    runHealth();
  }, [refresh, runHealth]);

  const windows = stats?.windows ?? {};
  const currentWindow = windows[activeTab] ?? windows.all ?? {};
  const fmt = (n: number) => (n ?? 0).toLocaleString();
  const reasonLabel = (r: string) => REASON_LABELS[r] ?? r;

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold">PXPIPE Token Saver</h1>
          <p className="text-sm opacity-70">
            Optional image-context compressor. Status:{" "}
            {status?.enabled ? "Enabled" : "Disabled"}
          </p>
        </div>
        <button
          onClick={refresh}
          className="rounded-md border px-3 py-1 text-sm"
        >
          Refresh
        </button>
      </div>

      {error && (
        <div className="rounded-md border border-red-500/40 bg-red-500/10 p-3 text-sm text-red-500">
          {error}
        </div>
      )}

      {/* Summary cards */}
      <div className="grid grid-cols-2 gap-3 md:grid-cols-3 lg:grid-cols-6">
        <SummaryCard label="Status" value={status?.enabled ? "Enabled" : "Disabled"} />
        <SummaryCard label="Version" value={status?.version ?? "—"} />
        <SummaryCard label="Uptime" value={`${Math.round((status?.uptimeMs ?? 0) / 1000)}s`} />
        <SummaryCard label="Requests" value={fmt(currentWindow.requests)} />
        <SummaryCard label="Compressed" value={fmt(currentWindow.compressed)} />
        <SummaryCard label="Bypassed" value={fmt(currentWindow.bypassed)} />
      </div>

      {/* Health */}
      {health && (
        <Card id="pxpipe-health">
          <Card.Section>
            <div className="flex items-center justify-between">
              <h2 className="text-sm font-medium">Health</h2>
              <span
                className={
                  health.healthy
                    ? "text-green-500"
                    : "text-red-500"
                }
              >
                {health.healthy ? "Healthy" : "Not healthy"}
              </span>
            </div>
            <ul className="mt-2 space-y-1 text-sm">
              {(health.checks ?? []).map((c: any) => (
                <li key={c.id} className="flex items-center gap-2">
                  <span className={c.ok ? "text-green-500" : "text-red-500"}>
                    {c.ok ? "✓" : "✗"}
                  </span>
                  <span>{c.label}</span>
                  {c.detail && (
                    <span className="opacity-60">— {c.detail}</span>
                  )}
                </li>
              ))}
            </ul>
          </Card.Section>
        </Card>
      )}

      {/* Tabs */}
      <div className="flex gap-2">
        {["all", "today", "yesterday", "last7d", "last30d"].map((tab) => (
          <button
            key={tab}
            onClick={() => setActiveTab(tab)}
            className={`rounded-md px-3 py-1 text-sm ${
              activeTab === tab
                ? "bg-emerald-500 text-white"
                : "border"
            }`}
          >
            {tab === "last7d" ? "7 days" : tab === "last30d" ? "30 days" : tab}
          </button>
        ))}
      </div>

      {/* Timeline chart */}
      <Card id="pxpipe-chart">
        <Card.Section>
          <h2 className="text-sm font-medium">Timeline</h2>
          <div className="mt-2 h-40 w-full">
            {stats?.timeline?.length ? (
              <InlineAreaChart data={stats.timeline} />
            ) : (
              <div className="flex h-full items-center justify-center text-sm opacity-50">
                No PXPIPE activity recorded
              </div>
            )}
          </div>
        </Card.Section>
      </Card>

      {/* History table */}
      <Card id="pxpipe-history">
        <Card.Section>
          <h2 className="text-sm font-medium">History</h2>
          <div className="mt-2 overflow-x-auto">
            <table className="w-full text-left text-sm">
              <thead>
                <tr className="opacity-60">
                  <th className="py-1 pr-3">Time</th>
                  <th className="py-1 pr-3">Provider</th>
                  <th className="py-1 pr-3">Model</th>
                  <th className="py-1 pr-3">Status</th>
                  <th className="py-1 pr-3">Reason</th>
                  <th className="py-1 pr-3">Saved</th>
                </tr>
              </thead>
              <tbody>
                {stats?.recent?.length ? (
                  stats.recent.map((ev: any, i: number) => (
                    <tr key={i} className="border-t border-white/10">
                      <td className="py-1 pr-3">{ev.ts ?? "—"}</td>
                      <td className="py-1 pr-3">{ev.provider ?? "—"}</td>
                      <td className="py-1 pr-3">{ev.model ?? "—"}</td>
                      <td className="py-1 pr-3">
                        <StatusBadge applied={ev.applied} />
                      </td>
                      <td className="py-1 pr-3">{reasonLabel(ev.reason ?? "")}</td>
                      <td className="py-1 pr-3">
                        {ev.tokensSavedEst != null
                          ? fmt(ev.tokensSavedEst)
                          : "—"}
                      </td>
                    </tr>
                  ))
                ) : (
                  <tr>
                    <td colSpan={6} className="py-3 text-center opacity-50">
                      No PXPIPE events yet
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
        </Card.Section>
      </Card>

      {/* Logs */}
      <Card id="pxpipe-logs">
        <Card.Section>
          <h2 className="text-sm font-medium">PXPIPE Logs</h2>
          <pre className="mt-2 max-h-60 overflow-auto rounded-md bg-black/30 p-3 text-xs">
            {logs?.installLog || "PXPIPE is not installed — no install log."}
          </pre>
        </Card.Section>
      </Card>
    </div>
  );
}

function SummaryCard({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-lg border p-3">
      <div className="text-xs opacity-60">{label}</div>
      <div className="mt-1 truncate text-sm font-semibold">{value}</div>
    </div>
  );
}

function StatusBadge({ applied }: { applied?: boolean }) {
  if (applied) {
    return <span className="text-green-500">Compressed</span>;
  }
  return <span className="text-amber-500">Bypassed</span>;
}

// Minimal inline SVG area chart matching 9router's #10b981 stroke.
function InlineAreaChart({ data }: { data: any[] }) {
  const w = 600;
  const h = 140;
  const pad = 4;
  const max = Math.max(1, ...data.map((d) => Number(d.tokensSavedEst ?? 0)));
  const points = data
    .map((d, i) => {
      const x = pad + (i * (w - pad * 2)) / Math.max(1, data.length - 1);
      const y = h - pad - (Number(d.tokensSavedEst ?? 0) / max) * (h - pad * 2);
      return `${x},${y}`;
    })
    .join(" ");
  const areaPoints = `${pad},${h - pad} ${points} ${w - pad},${h - pad}`;
  return (
    <svg viewBox={`0 0 ${w} ${h}`} className="h-full w-full" preserveAspectRatio="none">
      <defs>
        <linearGradient id="gradPxpipe" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="#10b981" stopOpacity="0.4" />
          <stop offset="100%" stopColor="#10b981" stopOpacity="0" />
        </linearGradient>
      </defs>
      <polygon points={areaPoints} fill="url(#gradPxpipe)" />
      <polyline
        points={points}
        fill="none"
        stroke="#10b981"
        strokeWidth="2"
        vectorEffect="non-scaling-stroke"
      />
    </svg>
  );
}
