"use client";

import { useState, useEffect } from "react";
import Card from "@/shared/components/Card";
import Loading from "@/shared/components/Loading";
import { SegmentedControl } from "@/shared/components";

interface RealUsageSources {
  provider: number;
  stream: number;
}

interface RealUsage {
  promptTokens: number;
  completionTokens: number;
  totalTokens: number;
  cacheTokens: number;
  sources: RealUsageSources;
}

interface ModeBreakdownEntry {
  mode: string;
  requests: number;
  tokensSaved: number;
}

interface CompressionStats {
  totalRequests: number;
  tokensSaved: number;
  avgSavingsPct: number;
  avgDurationMs: number;
  receipts: number;
  fallbacks: number;
  imagePrompts: number;
  realUsage: RealUsage;
  modeBreakdown: ModeBreakdownEntry[];
}

const PERIODS = [
  { value: "today", label: "Today" },
  { value: "24h", label: "24h" },
  { value: "7d", label: "7D" },
  { value: "30d", label: "30D" },
  { value: "all", label: "All" },
];

const compactFmt = new Intl.NumberFormat(undefined, {
  notation: "compact",
  maximumFractionDigits: 1,
});

const plainFmt = new Intl.NumberFormat(undefined, {
  maximumFractionDigits: 0,
});

const fmtCompact = (n: number) => compactFmt.format(n || 0);
const fmtNum = (n: number) => plainFmt.format(n || 0);
const fmtPct = (n: number) =>
  `${Number.isInteger(n) ? n : n.toFixed(1)}%`;
const fmtMs = (n: number) => `${Math.round(n || 0)}ms`;

export default function CompressionPageClient() {
  const [period, setPeriod] = useState("today");
  const [stats, setStats] = useState<CompressionStats | null>(null);
  const [loading, setLoading] = useState(true);
  const [fetching, setFetching] = useState(false);

  useEffect(() => {
    if (!stats) setLoading(true);
    else setFetching(true);

    fetch(`/api/compression/stats?period=${period}`)
      .then((r) => (r.ok ? r.json() : null))
      .then((data: CompressionStats | null) => {
        if (data) setStats(data);
      })
      .catch(() => {})
      .finally(() => {
        setLoading(false);
        setFetching(false);
      });
  }, [period]); // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <div className="flex min-w-0 flex-col gap-6 px-1 sm:px-0">
      {/* Header + period selector */}
      <div className="flex flex-col gap-3 sm:flex-row sm:items-end sm:justify-between">
        <div className="flex flex-col gap-1">
          <h1 className="text-2xl font-bold text-ink">Compression</h1>
          <p className="text-sm text-text-muted">
            Context compression analytics and token savings
          </p>
        </div>
        <SegmentedControl
          options={PERIODS}
          value={period}
          onChange={setPeriod}
          size="sm"
          className="w-full sm:w-auto"
        />
      </div>

      {loading ? (
        <Loading type="card" />
      ) : !stats ? (
        <div className="text-text-muted">Failed to load compression statistics.</div>
      ) : (
        <>
          {/* Summary cards */}
          <div className="grid min-w-0 grid-cols-1 gap-3 sm:grid-cols-2 md:grid-cols-3 xl:grid-cols-6">
            <Card className="flex min-w-0 flex-col gap-1 px-4 py-3">
              <span className="text-text-muted text-sm uppercase font-semibold">
                Total Requests
              </span>
              <span className="truncate text-2xl font-bold">{fmtNum(stats.totalRequests)}</span>
              <span className="text-[11px] text-text-muted">
                {fmtNum(stats.imagePrompts)} image prompts
              </span>
            </Card>
            <Card className="flex min-w-0 flex-col gap-1 px-4 py-3">
              <span className="text-text-muted text-sm uppercase font-semibold">
                Tokens Saved
              </span>
              <span className="truncate text-2xl font-bold text-[color:var(--color-success)]">
                {fmtCompact(stats.tokensSaved)}
              </span>
            </Card>
            <Card className="flex min-w-0 flex-col gap-1 px-4 py-3">
              <span className="text-text-muted text-sm uppercase font-semibold">
                % Avg Savings
              </span>
              <span className="truncate text-2xl font-bold text-[color:var(--color-warning)]">
                {fmtPct(stats.avgSavingsPct)}
              </span>
            </Card>
            <Card className="flex min-w-0 flex-col gap-1 px-4 py-3">
              <span className="text-text-muted text-sm uppercase font-semibold">
                Avg Duration
              </span>
              <span className="truncate text-2xl font-bold">{fmtMs(stats.avgDurationMs)}</span>
            </Card>
            <Card className="flex min-w-0 flex-col gap-1 px-4 py-3">
              <span className="text-text-muted text-sm uppercase font-semibold">
                Receipts
              </span>
              <span className="truncate text-2xl font-bold">{fmtNum(stats.receipts)}</span>
              <span className="text-[11px] text-text-muted">
                {fmtCompact(stats.realUsage.totalTokens)} real tokens
              </span>
            </Card>
            <Card className="flex min-w-0 flex-col gap-1 px-4 py-3">
              <span className="text-text-muted text-sm uppercase font-semibold">
                Fallbacks
              </span>
              <span className="truncate text-2xl font-bold text-[color:var(--color-danger)]">
                {fmtNum(stats.fallbacks)}
              </span>
              <span className="text-[11px] text-text-muted">validation restores</span>
            </Card>
          </div>

          {/* Real Usage Receipts */}
          <Card title="Real Usage Receipts" padding="md">
            <div className="grid min-w-0 grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-5">
              <ReceiptStat label="Prompt Tokens" value={fmtCompact(stats.realUsage.promptTokens)} />
              <ReceiptStat label="Completion Tokens" value={fmtCompact(stats.realUsage.completionTokens)} />
              <ReceiptStat label="Total Tokens" value={fmtCompact(stats.realUsage.totalTokens)} accent="success" />
              <ReceiptStat label="Cache Tokens" value={fmtCompact(stats.realUsage.cacheTokens)} accent="warning" />
              <div className="flex min-w-0 flex-col gap-1 rounded-mini-md bg-surface-card border border-hairline-soft px-4 py-3">
                <span className="text-text-muted text-xs uppercase font-semibold">Sources</span>
                <span className="truncate text-lg font-bold">
                  provider: {fmtNum(stats.realUsage.sources.provider)}
                </span>
                <span className="truncate text-sm text-text-muted">
                  stream: {fmtNum(stats.realUsage.sources.stream)}
                </span>
              </div>
            </div>
          </Card>

          {/* Mode Breakdown */}
          <Card title="Mode Breakdown" padding="md">
            {stats.modeBreakdown.length === 0 ? (
              <div className="text-text-muted text-sm py-4">No mode data recorded yet.</div>
            ) : (
              <div className="overflow-x-auto">
                <table className="w-full min-w-[360px] border-collapse text-sm">
                  <thead>
                    <tr className="border-b border-hairline-soft">
                      <th className="py-2 text-left font-semibold text-text-muted">Mode</th>
                      <th className="py-2 text-right font-semibold text-text-muted">Requests</th>
                      <th className="py-2 text-right font-semibold text-text-muted">Tokens Saved</th>
                    </tr>
                  </thead>
                  <tbody className="divide-y divide-hairline-soft">
                    {stats.modeBreakdown.map((entry) => (
                      <tr key={entry.mode} className="hover:bg-surface-soft transition-colors">
                        <td className="py-2 font-medium text-ink">{entry.mode}</td>
                        <td className="py-2 text-right text-text-muted">{fmtNum(entry.requests)}</td>
                        <td className="py-2 text-right font-semibold text-[color:var(--color-success)]">
                          {fmtCompact(entry.tokensSaved)}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </Card>
        </>
      )}
    </div>
  );
}

function ReceiptStat({
  label,
  value,
  accent,
}: {
  label: string;
  value: string;
  accent?: "success" | "warning";
}) {
  const accentClass =
    accent === "success"
      ? "text-[color:var(--color-success)]"
      : accent === "warning"
        ? "text-[color:var(--color-warning)]"
        : "";
  return (
    <div className="flex min-w-0 flex-col gap-1 rounded-mini-md bg-surface-card border border-hairline-soft px-4 py-3">
      <span className="text-text-muted text-xs uppercase font-semibold">{label}</span>
      <span className={`truncate text-lg font-bold ${accentClass}`}>{value}</span>
    </div>
  );
}
