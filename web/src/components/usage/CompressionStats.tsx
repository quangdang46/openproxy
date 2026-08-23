"use client";

import { useState, useEffect, useMemo } from "react";
import {
  formatTokens,
  formatNumber,
  formatPercent,
  USAGE_COLORS,
  periodToBackend,
} from "./usageFormat";

const COMPRESSION_PERIODS = [
  { value: "today", label: "Last 24h" },
  { value: "7d", label: "7d" },
  { value: "30d", label: "30d" },
  { value: "all", label: "All time" },
];

function KpiCard({
  label,
  value,
  sub,
  icon,
}: {
  label: string;
  value: string;
  sub?: string;
  icon: string;
}) {
  return (
    <div className="flex flex-col gap-2 rounded-mini-xl border border-border bg-surface-2 p-5">
      <div className="flex items-center justify-between">
        <span className="text-[11px] font-semibold uppercase tracking-wider text-text-muted">
          {label}
        </span>
        <span className="material-symbols-outlined text-[18px] text-text-muted">{icon}</span>
      </div>
      <span className="text-2xl font-bold tabular-nums sm:text-3xl text-text">{value}</span>
      {sub && <span className="text-xs text-text-muted">{sub}</span>}
    </div>
  );
}

export default function CompressionStats({ period = "30d" }: { period?: string }) {
  const [stats, setStats] = useState<any>(null);
  const [daily, setDaily] = useState<any[]>([]);
  const [loading, setLoading] = useState(true);
  const [localPeriod, setLocalPeriod] = useState(period);

  useEffect(() => setLocalPeriod(period), [period]);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    Promise.all([
      fetch(`/api/usage/stats?period=${periodToBackend(localPeriod)}`).then((r) =>
        r.ok ? r.json() : null
      ),
      fetch(`/api/usage/daily`).then((r) => (r.ok ? r.json() : null)),
    ])
      .then(([s, d]) => {
        if (cancelled) return;
        if (s) setStats(s);
        if (Array.isArray(d)) setDaily(d);
      })
      .catch(() => {})
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [localPeriod]);

  // OpenProxy records prompt-cache reads but not RTK receipt-level savings yet.
  // We surface cache-read tokens as the available "tokens saved" proxy and mark
  // the page clearly as partial until receipt-level compression tracking lands.
  const derived = useMemo(() => {
    const prompt = stats?.totalPromptTokens || 0;
    const completion = stats?.totalCompletionTokens || 0;
    const cacheRead = stats?.totalCacheReadInputTokens || 0;
    const requests = stats?.totalRequests || 0;
    const tokensSaved = cacheRead;
    const realTokens = prompt + completion + cacheRead;
    const avgSavings = realTokens > 0 ? (tokensSaved / realTokens) * 100 : 0;
    const receipts = daily.filter(
      (d: any) => (d.promptTokens || 0) + (d.completionTokens || 0) > 0
    ).length;
    return { prompt, completion, cacheRead, requests, tokensSaved, realTokens, avgSavings, receipts };
  }, [stats, daily]);

  return (
    <div className="flex min-w-0 flex-col gap-6">
      {/* Header + breadcrumb */}
      <div className="flex flex-col gap-1">
        <nav className="flex items-center gap-1.5 text-xs text-text-muted">
          <span>Dashboard</span>
          <span className="opacity-50">›</span>
          <span>Analytics</span>
          <span className="opacity-50">›</span>
          <span className="text-text">Compression</span>
        </nav>
        <div className="flex flex-col gap-3 sm:flex-row sm:items-end sm:justify-between">
          <div>
            <h2 className="text-lg font-semibold text-text">Compression</h2>
            <p className="text-sm text-text-muted">
              Token savings from prompt-cache reads and RTK compression.
            </p>
          </div>
          <div className="flex items-center gap-1">
            {COMPRESSION_PERIODS.map((p) => {
              const active = localPeriod === p.value;
              return (
                <button
                  key={p.value}
                  onClick={() => setLocalPeriod(p.value)}
                  className="rounded-mini-lg px-3 py-1.5 text-xs font-semibold transition-colors"
                  style={
                    active
                      ? { backgroundColor: USAGE_COLORS.active, color: "#fff" }
                      : { color: "var(--color-text-muted)" }
                  }
                >
                  {p.label}
                </button>
              );
            })}
          </div>
        </div>
      </div>

      <div className="rounded-mini-md border border-border bg-surface-base/60 px-3 py-2 text-xs text-text-muted">
        Compression tracking is partial — <span className="text-text">Tokens Saved</span> is
        derived from prompt-cache reads. RTK receipt-level metrics (duration, per-receipt
        savings) are coming soon.
      </div>

      {loading ? (
        <div className="flex items-center justify-center py-16 text-text-muted">
          <span className="material-symbols-outlined animate-spin text-3xl">progress_activity</span>
        </div>
      ) : (
        <>
          {/* KPI row */}
          <div className="grid grid-cols-2 gap-4 lg:grid-cols-5">
            <KpiCard label="Total Requests" value={formatNumber(derived.requests)} icon="send" />
            <KpiCard label="Tokens Saved" value={formatTokens(derived.tokensSaved)} icon="compress" />
            <KpiCard label="Avg Savings" value={formatPercent(derived.avgSavings)} icon="trending_down" />
            <KpiCard label="Avg Duration" value="—" icon="timer" />
            <KpiCard
              label="Receipts"
              value={formatNumber(derived.receipts)}
              sub={`${formatTokens(derived.realTokens)} real tokens`}
              icon="receipt_long"
            />
          </div>

          {/* Real Usage Receipts */}
          <div className="rounded-mini-xl border border-border bg-surface-2 p-5">
            <h3 className="mb-4 text-[11px] font-semibold uppercase tracking-widest text-text-muted">
              Real Usage Receipts
            </h3>
            <div className="grid grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-5">
              <div className="flex flex-col gap-1">
                <span className="text-[11px] uppercase tracking-wider text-text-muted">Prompt Tokens</span>
                <span className="text-lg font-semibold text-text">{formatTokens(derived.prompt)}</span>
              </div>
              <div className="flex flex-col gap-1">
                <span className="text-[11px] uppercase tracking-wider text-text-muted">Completion Tokens</span>
                <span className="text-lg font-semibold text-text">{formatTokens(derived.completion)}</span>
              </div>
              <div className="flex flex-col gap-1">
                <span className="text-[11px] uppercase tracking-wider text-text-muted">Cache Read</span>
                <span className="text-lg font-semibold text-text">{formatTokens(derived.cacheRead)}</span>
              </div>
              <div className="flex flex-col gap-1">
                <span className="text-[11px] uppercase tracking-wider text-text-muted">Sources · Provider</span>
                <span className="text-lg font-semibold text-text">{formatNumber(derived.requests)}</span>
              </div>
              <div className="flex flex-col gap-1">
                <span className="text-[11px] uppercase tracking-wider text-text-muted">Sources · Stream</span>
                <span className="text-lg font-semibold text-text">0</span>
              </div>
            </div>
          </div>

          {/* Mode Breakdown */}
          <div className="rounded-mini-xl border border-border bg-surface-2 p-5">
            <h3 className="mb-4 text-[11px] font-semibold uppercase tracking-widest text-text-muted">
              Mode Breakdown
            </h3>
            <div className="flex flex-col gap-3">
              <div>
                <div className="mb-1.5 flex items-center justify-between text-sm">
                  <span className="font-medium text-text">Standard</span>
                  <span className="text-text-muted">
                    {formatNumber(derived.requests)} requests · {formatTokens(derived.tokensSaved)} tokens saved
                  </span>
                </div>
                <div className="h-2 w-full overflow-hidden rounded-full bg-border">
                  <div
                    className="h-full rounded-full"
                    style={{
                      width: derived.requests > 0 ? "100%" : "0%",
                      backgroundColor: USAGE_COLORS.active,
                    }}
                  />
                </div>
              </div>
            </div>
          </div>
        </>
      )}
    </div>
  );
}
