"use client";

import React, { useCallback, useEffect, useState } from "react";
import {
  AreaChart,
  Area,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
} from "recharts";
import { Card, Button } from "@/shared/components";

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

const fmtTokens = (n: number) => {
  if (n >= 1000000) return `${(n / 1000000).toFixed(2)}M`;
  if (n >= 1000) return `${(n / 1000).toFixed(1)}K`;
  return String(n || 0);
};

const fmtUptime = (ms: number) => {
  if (!ms || ms <= 0) return "—";
  const m = Math.floor(ms / 60000);
  const h = Math.floor(m / 60);
  return h > 0 ? `${h}h${String(m % 60).padStart(2, "0")}m` : `${m}m`;
};

const WINDOW_TABS = [
  { id: "today", label: "Today" },
  { id: "yesterday", label: "Yesterday" },
  { id: "last7d", label: "7 days" },
  { id: "last30d", label: "30 days" },
  { id: "all", label: "All time" },
];

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

function SummaryCard({ label, value, sub, tone }: { label: string; value: string; sub?: string; tone?: string }) {
  return (
    <Card className="p-4">
      <p className="text-xs text-text-muted uppercase tracking-wide">{label}</p>
      <p className={`text-xl font-semibold mt-1 ${tone || ""}`}>{value}</p>
      {sub && <p className="text-xs text-text-muted mt-0.5">{sub}</p>}
    </Card>
  );
}

function StatusBadge({ applied, reason, detail }: { applied?: boolean; reason?: string; detail?: string }) {
  if (applied) {
    return <span className="bg-success/15 text-success text-xs px-2 py-0.5 rounded">Compressed</span>;
  }
  const danger = reason === "transform_error" || reason === "timeout";
  return (
    <span
      className={`text-xs px-2 py-0.5 rounded ${danger ? "bg-danger/15 text-danger" : "bg-warning/15 text-warning"}`}
      title={detail || ""}
    >
      {reason ? REASON_LABELS[reason] || reason : "Bypassed"}
    </span>
  );
}

export default function PxpipePageClient() {
  const [status, setStatus] = useState<PxpipeStatus | null>(null);
  const [stats, setStats] = useState<any>(null);
  const [logs, setLogs] = useState<PxpipeLogs | null>(null);
  const [health, setHealth] = useState<any>(null);
  const [windowId, setWindowId] = useState("last7d");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const [statusRes, statsRes, logsRes] = await Promise.all([
        fetch("/api/pxpipe/status", { headers: { "Cache-Control": "no-store" } }),
        fetch("/api/pxpipe/stats"),
        fetch("/api/pxpipe/logs?limit=50"),
      ]);
      setStatus(await statusRes.json());
      setStats(await statsRes.json());
      setLogs(await logsRes.json());
      const healthRes = await fetch("/api/pxpipe/health", { method: "POST" });
      setHealth(await healthRes.json());
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const w = stats?.windows?.[windowId];
  const statusLabel = !status
    ? "—"
    : !status.installed
      ? "Not installed"
      : health?.healthy
        ? "Healthy"
        : status.running
          ? "Running"
          : "Stopped";

  return (
    <div className="space-y-6 p-6">
      <div className="flex items-center justify-between flex-wrap gap-3">
        <h2 className="text-lg font-semibold flex items-center gap-2">
          <span className="material-symbols-outlined text-primary">image</span>
          PXPIPE Dashboard
        </h2>
        <div className="flex items-center gap-2">
          <a href="/dashboard/token-saver" className="text-xs text-primary underline hover:opacity-80">
            Token Saver settings
          </a>
          <Button size="sm" variant="ghost" onClick={refresh} disabled={loading}>
            {loading ? "Refreshing…" : "Refresh"}
          </Button>
        </div>
      </div>

      {error && (
        <div className="rounded-md border border-red-500/40 bg-red-500/10 p-3 text-sm text-red-500">
          {error}
        </div>
      )}

      <div className="grid grid-cols-2 md:grid-cols-3 lg:grid-cols-6 gap-3">
        <SummaryCard
          label="Status"
          value={statusLabel}
          tone={health?.healthy ? "text-success" : status?.installed ? "text-warning" : "text-text-muted"}
          sub={status?.enabled ? "Enabled in pipeline" : "Disabled in pipeline"}
        />
        <SummaryCard label="Version" value={status?.version ? `v${status.version}` : "—"} sub="pxpipe-proxy" />
        <SummaryCard label="Uptime" value={fmtUptime(status?.uptimeMs ?? 0)} sub="module loaded" />
        <SummaryCard label="Requests" value={w ? w.requests.toLocaleString() : "—"} />
        <SummaryCard label="Compressed" value={w ? w.compressed.toLocaleString() : "—"} tone="text-success" />
        <SummaryCard label="Bypassed" value={w ? w.bypassed.toLocaleString() : "—"} />
      </div>

      {/* Web's own Health block (kept — richer than 9router) */}
      {health && (
        <Card id="pxpipe-health">
          <Card.Section>
            <div className="flex items-center justify-between">
              <h2 className="text-sm font-medium">Health</h2>
              <span className={health.healthy ? "text-green-500" : "text-red-500"}>
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
                  {c.detail && <span className="opacity-60">— {c.detail}</span>}
                </li>
              ))}
            </ul>
          </Card.Section>
        </Card>
      )}

      <Card className="p-4">
        <div className="flex items-center justify-between flex-wrap gap-3 mb-4">
          <h3 className="font-medium">Token savings (estimated)</h3>
          <div className="flex items-center gap-1 rounded-lg border border-border bg-bg-subtle p-1">
            {WINDOW_TABS.map((tab) => (
              <button
                key={tab.id}
                onClick={() => setWindowId(tab.id)}
                className={`px-3 py-1 rounded-md text-xs font-medium transition-colors ${
                  windowId === tab.id
                    ? "bg-primary text-white shadow-sm"
                    : "text-text-muted hover:text-text hover:bg-bg-hover"
                }`}
              >
                {tab.label}
              </button>
            ))}
          </div>
        </div>
        <div className="grid grid-cols-2 md:grid-cols-4 gap-4 text-center">
          <div>
            <p className="text-xs text-text-muted">Original tokens</p>
            <p className="text-lg font-semibold">{w ? fmtTokens(w.tokensBeforeEst) : "—"}</p>
          </div>
          <div>
            <p className="text-xs text-text-muted">After PXPIPE</p>
            <p className="text-lg font-semibold">{w ? fmtTokens(w.tokensAfterEst) : "—"}</p>
          </div>
          <div>
            <p className="text-xs text-text-muted">Saved</p>
            <p className="text-lg font-semibold text-success">{w ? fmtTokens(w.tokensSavedEst) : "—"}</p>
          </div>
          <div>
            <p className="text-xs text-text-muted">Reduction</p>
            <p className="text-lg font-semibold text-success">{w ? `${w.savedPct}%` : "—"}</p>
          </div>
        </div>
        <p className="text-xs text-text-muted mt-3">
          Estimates from body size before/after imaging; billed usage per request
          (recorded on the Usage page) remains the ground truth. Images generated:{" "}
          {w ? w.imagesGenerated.toLocaleString() : "—"} · avg compression time:{" "}
          {w ? `${w.avgCompressionMs}ms` : "—"} · errors: {w ? w.errors : "—"}
        </p>
      </Card>

      <Card className="p-4">
        <h3 className="font-medium mb-3">Tokens saved — last 30 days</h3>
        {stats?.timeline?.some((d: any) => d.tokensSavedEst > 0) ? (
          <ResponsiveContainer width="100%" height={220}>
            <AreaChart data={stats.timeline} margin={{ top: 4, right: 8, left: 0, bottom: 0 }}>
              <defs>
                <linearGradient id="gradPxpipe" x1="0" y1="0" x2="0" y2="1">
                  <stop offset="5%" stopColor="#10b981" stopOpacity={0.25} />
                  <stop offset="95%" stopColor="#10b981" stopOpacity={0} />
                </linearGradient>
              </defs>
              <CartesianGrid strokeDasharray="3 3" strokeOpacity={0.2} />
              <XAxis dataKey="date" tick={{ fontSize: 11 }} tickFormatter={(d: any) => String(d).slice(5)} />
              <YAxis tick={{ fontSize: 11 }} tickFormatter={(v: any) => fmtTokens(Number(v))} width={48} />
              <Tooltip formatter={(v: any) => [fmtTokens(Number(v)), "Tokens saved"]} labelFormatter={(d: any) => String(d)} />
              <Area type="monotone" dataKey="tokensSavedEst" stroke="#10b981" fill="url(#gradPxpipe)" strokeWidth={2} />
            </AreaChart>
          </ResponsiveContainer>
        ) : (
          <div className="h-32 flex items-center justify-center text-text-muted text-sm">
            No savings recorded yet — enable PXPIPE in the Token Saver and route a large Claude-format request.
          </div>
        )}
      </Card>

      <Card className="p-4">
        <h3 className="font-medium mb-3">History</h3>
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr className="text-left text-xs text-text-muted border-b border-border">
                <th className="py-2 pr-3">Time</th>
                <th className="py-2 pr-3">Model</th>
                <th className="py-2 pr-3 text-right">Original</th>
                <th className="py-2 pr-3 text-right">Compressed</th>
                <th className="py-2 pr-3 text-right">Saved</th>
                <th className="py-2 pr-3 text-right">%</th>
                <th className="py-2 pr-3 text-right">Duration</th>
                <th className="py-2">Status</th>
              </tr>
            </thead>
            <tbody>
              {(stats?.recent || []).slice(0, 50).map((ev: any, i: number) => (
                <tr key={`${ev.ts}-${i}`} className="border-b border-border/50">
                  <td className="py-1.5 pr-3 whitespace-nowrap text-text-muted">
                    {ev.ts ? new Date(ev.ts).toLocaleString() : "—"}
                  </td>
                  <td className="py-1.5 pr-3 font-mono text-xs">{ev.provider ? `${ev.provider}/${ev.model}` : ev.model || "—"}</td>
                  <td className="py-1.5 pr-3 text-right font-mono text-xs">
                    {ev.applied ? fmtTokens(ev.tokensBeforeEst) : "—"}
                  </td>
                  <td className="py-1.5 pr-3 text-right font-mono text-xs">
                    {ev.applied ? fmtTokens(ev.tokensAfterEst) : "—"}
                  </td>
                  <td className="py-1.5 pr-3 text-right font-mono text-xs text-success">
                    {ev.applied ? fmtTokens(ev.tokensSavedEst) : "—"}
                  </td>
                  <td className="py-1.5 pr-3 text-right font-mono text-xs">
                    {ev.applied ? `${ev.savedPct}%` : "—"}
                  </td>
                  <td className="py-1.5 pr-3 text-right font-mono text-xs">
                    {ev.durationMs != null ? `${ev.durationMs}ms` : "—"}
                  </td>
                  <td className="py-1.5">
                    <StatusBadge applied={ev.applied} reason={ev.reason} detail={ev.detail} />
                  </td>
                </tr>
              ))}
              {(!stats?.recent || stats.recent.length === 0) && (
                <tr>
                  <td colSpan={8} className="py-6 text-center text-text-muted text-sm">
                    No PXPIPE activity yet
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      </Card>

      <Card className="p-4" id="logs">
        <h3 className="font-medium mb-3">PXPIPE Logs</h3>
        {logs?.installLog ? (
          <pre className="rounded bg-black/5 dark:bg-white/5 p-3 text-xs font-mono overflow-x-auto max-h-64 overflow-y-auto whitespace-pre-wrap">
            {logs.installLog}
          </pre>
        ) : (
          <p className="text-sm text-text-muted">No install log yet.</p>
        )}
      </Card>
    </div>
  );
}
