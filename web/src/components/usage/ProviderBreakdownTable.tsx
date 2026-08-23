"use client";

import { useState, useEffect, useMemo } from "react";
import {
  formatTokens,
  formatCost,
  formatNumber,
  providerColor,
  providerName,
  USAGE_COLORS,
  periodToBackend,
} from "./usageFormat";

interface AggStats {
  requests?: number;
  promptTokens?: number;
  completionTokens?: number;
  reasoningTokens?: number;
  cachedTokens?: number;
  cacheReadInputTokens?: number;
  cacheCreationInputTokens?: number;
  cost?: number;
}

interface ProviderRow {
  provider: string;
  requests: number;
  input: number;
  output: number;
  total: number;
  cost: number;
  share: number;
  color: string;
}

type SortKey =
  | "provider"
  | "requests"
  | "input"
  | "output"
  | "total"
  | "cost"
  | "share";

const PERIODS = [
  { value: "today", label: "1D" },
  { value: "7d", label: "7D" },
  { value: "30d", label: "30D" },
  { value: "60d", label: "90D" },
  { value: "all", label: "All" },
];

function SortGlyph({ active, order }: { active: boolean; order: "asc" | "desc" }) {
  if (!active) return <span className="ml-1 opacity-30">↕</span>;
  return <span className="ml-1">{order === "asc" ? "↑" : "↓"}</span>;
}

export default function ProviderBreakdownTable({
  period = "30d",
}: {
  period?: string;
}) {
  const [byProvider, setByProvider] = useState<Record<string, AggStats> | null>(null);
  const [loading, setLoading] = useState(true);
  const [sortKey, setSortKey] = useState<SortKey>("total");
  const [sortOrder, setSortOrder] = useState<"asc" | "desc">("desc");
  const [localPeriod, setLocalPeriod] = useState(period);

  useEffect(() => {
    setLocalPeriod(period);
  }, [period]);

  useEffect(() => {
    setLoading(true);
    let cancelled = false;
    fetch(`/api/usage/stats?period=${periodToBackend(localPeriod)}`)
      .then((r) => (r.ok ? r.json() : null))
      .then((d) => {
        if (!cancelled && d) setByProvider(d.byProvider || {});
      })
      .catch(() => {})
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [localPeriod]);

  const totalCost = useMemo(
    () =>
      Object.values(byProvider || {}).reduce(
        (s, v) => s + (v.cost || 0),
        0
      ),
    [byProvider]
  );

  const rows = useMemo(() => {
    const arr: ProviderRow[] = Object.entries(byProvider || {}).map(
      ([provider, v], i) => {
        const input = v.promptTokens || 0;
        const output = v.completionTokens || 0;
        return {
          provider,
          requests: v.requests || 0,
          input,
          output,
          total: input + output,
          cost: v.cost || 0,
          share: totalCost > 0 ? ((v.cost || 0) / totalCost) * 100 : 0,
          color: providerColor(provider, i),
        };
      }
    );
    const dir = sortOrder === "asc" ? 1 : -1;
    arr.sort((a, b) => {
      const av = a[sortKey] as any;
      const bv = b[sortKey] as any;
      if (typeof av === "string") {
        return av.toLowerCase() < bv.toLowerCase() ? -dir : av.toLowerCase() > bv.toLowerCase() ? dir : 0;
      }
      if (av < bv) return -dir;
      if (av > bv) return dir;
      return 0;
    });
    return arr;
  }, [byProvider, totalCost, sortKey, sortOrder]);

  const toggleSort = (key: SortKey) => {
    if (key === sortKey) {
      setSortOrder((o) => (o === "asc" ? "desc" : "asc"));
    } else {
      setSortKey(key);
      setSortOrder(key === "provider" ? "asc" : "desc");
    }
  };

  const headers: { key: SortKey; label: string; align: "left" | "right"; color?: string }[] = [
    { key: "provider", label: "Provider", align: "left" },
    { key: "requests", label: "Requests", align: "right" },
    { key: "input", label: "Input", align: "right", color: USAGE_COLORS.input },
    { key: "output", label: "Output", align: "right", color: USAGE_COLORS.output },
    { key: "total", label: "Total", align: "right", color: USAGE_COLORS.total },
    { key: "cost", label: "Cost", align: "right", color: USAGE_COLORS.cost },
    { key: "share", label: "Share", align: "right" },
  ];

  return (
    <div className="flex min-w-0 flex-col gap-4">
      <div className="flex items-center justify-between gap-3">
        <h2 className="text-xs font-semibold uppercase tracking-widest text-text-muted">
          Provider Breakdown
        </h2>
        <div className="flex items-center gap-1 rounded-mini-md border border-border bg-surface-base p-1">
          {PERIODS.map((p) => (
            <button
              key={p.value}
              onClick={() => setLocalPeriod(p.value)}
              className={`rounded-mini-sm px-2.5 py-1 text-xs font-medium transition-colors ${
                localPeriod === p.value
                  ? "bg-ink text-canvas"
                  : "text-text-muted hover:text-text"
              }`}
            >
              {p.label}
            </button>
          ))}
        </div>
      </div>

      <div className="overflow-hidden rounded-mini-lg border border-border bg-surface-2">
        <div className="overflow-x-auto">
          <table className="w-full min-w-[640px] text-sm">
            <thead>
              <tr className="border-b border-border text-[11px] uppercase tracking-wider text-text-muted">
                {headers.map((h) => (
                  <th
                    key={h.key}
                    onClick={() => toggleSort(h.key)}
                    className={`cursor-pointer select-none px-4 py-3 font-semibold hover:text-text ${
                      h.align === "right" ? "text-right" : "text-left"
                    }`}
                  >
                    <span className={h.color ? "text-[inherit]" : ""} style={h.color ? { color: h.color } : undefined}>
                      {h.label}
                    </span>
                    <SortGlyph active={sortKey === h.key} order={sortOrder} />
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {loading ? (
                <tr>
                  <td colSpan={headers.length} className="px-4 py-10 text-center text-text-muted">
                    <span className="material-symbols-outlined inline-block animate-spin text-xl">
                      progress_activity
                    </span>
                  </td>
                </tr>
              ) : rows.length === 0 ? (
                <tr>
                  <td colSpan={headers.length} className="px-4 py-10 text-center text-text-muted">
                    No usage recorded yet.
                  </td>
                </tr>
              ) : (
                rows.map((row, i) => (
                  <tr
                    key={row.provider}
                    className={`border-b border-border/60 transition-colors hover:bg-surface-3/50 ${
                      i % 2 === 1 ? "bg-surface-base/30" : ""
                    }`}
                  >
                    <td className="px-4 py-3">
                      <div className="flex items-center gap-2.5">
                        <span
                          className="h-2 w-2 shrink-0 rounded-full"
                          style={{ backgroundColor: row.color }}
                        />
                        <span className="font-medium text-text">{providerName(row.provider)}</span>
                      </div>
                    </td>
                    <td className="px-4 py-3 text-right text-text-muted">
                      {formatNumber(row.requests)}
                    </td>
                    <td className="px-4 py-3 text-right" style={{ color: USAGE_COLORS.input }}>
                      {formatTokens(row.input)}
                    </td>
                    <td className="px-4 py-3 text-right" style={{ color: USAGE_COLORS.output }}>
                      {formatTokens(row.output)}
                    </td>
                    <td className="px-4 py-3 text-right font-bold" style={{ color: USAGE_COLORS.total }}>
                      {formatTokens(row.total)}
                    </td>
                    <td className="px-4 py-3 text-right" style={{ color: USAGE_COLORS.cost }}>
                      {formatCost(row.cost)}
                    </td>
                    <td className="px-4 py-3">
                      <div className="flex items-center justify-end gap-2">
                        <div className="h-1.5 w-20 overflow-hidden rounded-full bg-border">
                          <div
                            className="h-full rounded-full"
                            style={{
                              width: `${Math.max(2, Math.min(100, row.share))}%`,
                              backgroundColor: row.color,
                            }}
                          />
                        </div>
                        <span className="w-10 text-right text-xs tabular-nums text-text-muted">
                          {row.share.toFixed(1)}%
                        </span>
                      </div>
                    </td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  );
}
