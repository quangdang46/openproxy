"use client";

import { useState, useEffect, useMemo } from "react";
import {
  formatTokens,
  formatCost,
  formatNumber,
  formatPercent,
  providerName,
  USAGE_COLORS,
  periodToBackend,
} from "./usageFormat";

interface StatsPayload {
  totalRequests?: number;
  totalPromptTokens?: number;
  totalCompletionTokens?: number;
  totalReasoningTokens?: number;
  totalCachedTokens?: number;
  totalCacheReadInputTokens?: number;
  totalCost?: number;
  byProvider?: Record<string, AggStats>;
  byModel?: Record<string, ModelStats>;
  byAccount?: Record<string, any>;
  byApiKey?: Record<string, any>;
}

interface AggStats {
  requests?: number;
  promptTokens?: number;
  completionTokens?: number;
  cost?: number;
}
interface ModelStats {
  requests?: number;
  promptTokens?: number;
  completionTokens?: number;
  cost?: number;
  rawModel?: string;
  provider?: string;
}
interface DailyDay {
  date: string;
  requests?: number;
  promptTokens?: number;
  completionTokens?: number;
  cost?: number;
}

const ANALYTICS_PERIODS = [
  { value: "today", label: "1D" },
  { value: "7d", label: "7D" },
  { value: "30d", label: "30D" },
  { value: "60d", label: "90D" },
  { value: "all", label: "All" },
];

function KpiCard({
  label,
  value,
  sub,
  color,
  icon,
}: {
  label: string;
  value: string;
  sub?: string;
  color?: string;
  icon: string;
}) {
  return (
    <div className="flex flex-col gap-2 rounded-mini-xl border border-border bg-surface-2 p-5">
      <div className="flex items-center justify-between">
        <span className="text-[11px] font-semibold uppercase tracking-wider text-text-muted">
          {label}
        </span>
        <span className="material-symbols-outlined text-[18px] text-text-muted">
          {icon}
        </span>
      </div>
      <span
        className="text-2xl font-bold tabular-nums sm:text-3xl"
        style={color ? { color } : undefined}
      >
        {value}
      </span>
      {sub && <span className="text-xs text-text-muted">{sub}</span>}
    </div>
  );
}

function StatCell({
  label,
  value,
  color,
  link,
}: {
  label: string;
  value: string;
  color?: string;
  link?: boolean;
}) {
  return (
    <div className="flex flex-col gap-1">
      <span className="text-[11px] uppercase tracking-wider text-text-muted">{label}</span>
      <span
        className={`text-lg font-semibold ${link ? "cursor-pointer hover:underline" : ""}`}
        style={color ? { color } : undefined}
      >
        {value}
      </span>
    </div>
  );
}

function SectionTitle({ children }: { children: React.ReactNode }) {
  return (
    <h3 className="mb-3 text-[11px] font-semibold uppercase tracking-widest text-text-muted">
      {children}
    </h3>
  );
}

// Build a GitHub-style 53-week x 7-day grid ending today.
function buildHeatmap(daily: DailyDay[]) {
  const map = new Map<string, number>();
  let max = 0;
  for (const d of daily) {
    const tokens = (d.promptTokens || 0) + (d.completionTokens || 0);
    map.set(d.date, tokens);
    if (tokens > max) max = tokens;
  }
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const end = new Date(today);
  const start = new Date(today);
  start.setDate(start.getDate() - 52 * 7);
  // align start back to Sunday
  start.setDate(start.getDate() - start.getDay());

  const weeks: { date: string; tokens: number; inRange: boolean }[][] = [];
  const cursor = new Date(start);
  const monthLabels: { col: number; label: string }[] = [];
  let lastMonth = -1;
  let col = 0;
  while (cursor <= end) {
    const week: { date: string; tokens: number; inRange: boolean }[] = [];
    for (let day = 0; day < 7; day++) {
      const iso = cursor.toISOString().slice(0, 10);
      const inRange = cursor <= end;
      week.push({ date: iso, tokens: map.get(iso) || 0, inRange });
      if (day === 0) {
        const m = cursor.getMonth();
        if (m !== lastMonth) {
          monthLabels.push({ col, label: cursor.toLocaleString("en-US", { month: "short" }) });
          lastMonth = m;
        }
      }
      cursor.setDate(cursor.getDate() + 1);
    }
    weeks.push(week);
    col++;
  }
  return { weeks, max, monthLabels };
}

function heatColor(tokens: number, max: number): string {
  if (tokens <= 0 || max <= 0) return "rgba(255,255,255,0.04)";
  const t = Math.min(1, tokens / max);
  // interpolate from dark to red
  if (t < 0.25) return "#3a1717";
  if (t < 0.5) return "#7f1d1d";
  if (t < 0.75) return "#b91c1c";
  return "#ef4444";
}

export default function UsageAnalyticsGrid({ period = "30d" }: { period?: string }) {
  const [stats, setStats] = useState<StatsPayload | null>(null);
  const [daily, setDaily] = useState<DailyDay[]>([]);
  const [infra, setInfra] = useState({ accounts: 0, providers: 0, keys: 0, models: 0 });
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
      fetch(`/api/providers`).then((r) => (r.ok ? r.json() : null)),
      fetch(`/api/keys`).then((r) => (r.ok ? r.json() : null)),
      fetch(`/api/provider-models`).then((r) => (r.ok ? r.json() : null)),
    ])
      .then(([s, d, p, k, m]) => {
        if (cancelled) return;
        if (s) setStats(s);
        if (Array.isArray(d)) setDaily(d);
        // infra
        const connections = p?.connections || [];
        const providers = new Set(connections.map((c: any) => c.provider).filter(Boolean));
        const keysArr = Array.isArray(k) ? k : k?.keys || [];
        const modelsArr = Array.isArray(m) ? m : m?.models || m?.data || [];
        setInfra({
          accounts: connections.length,
          providers: providers.size,
          keys: keysArr.length,
          models: modelsArr.length,
        });
      })
      .catch(() => {})
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [localPeriod]);

  const totals = useMemo(() => {
    const prompt = stats?.totalPromptTokens || 0;
    const completion = stats?.totalCompletionTokens || 0;
    const total = prompt + completion + (stats?.totalReasoningTokens || 0);
    const requests = stats?.totalRequests || 0;
    const cost = stats?.totalCost || 0;
    return { prompt, completion, total, requests, cost };
  }, [stats]);

  const topProvider = useMemo(() => {
    const entries = Object.entries(stats?.byProvider || {});
    if (!entries.length) return null;
    entries.sort((a, b) => (b[1].cost || 0) - (a[1].cost || 0));
    return entries[0];
  }, [stats]);

  const topModel = useMemo(() => {
    const entries = Object.entries(stats?.byModel || {});
    if (!entries.length) return null;
    entries.sort((a, b) => (b[1].cost || 0) - (a[1].cost || 0));
    const [key, v] = entries[0];
    const raw = v.rawModel || key.replace(/\s*\(.*\)$/, "");
    return { name: raw, provider: v.provider || "" };
  }, [stats]);

  const diversity = useMemo(() => {
    const costs = Object.values(stats?.byProvider || {}).map((v) => v.cost || 0);
    const sum = costs.reduce((a, b) => a + b, 0);
    if (sum <= 0) return 0;
    const hhi = costs.reduce((a, c) => a + Math.pow(c / sum, 2), 0);
    return (1 - hhi) * 100;
  }, [stats]);

  const busiest = useMemo(() => {
    if (!daily.length) return null;
    let best: DailyDay | null = null;
    for (const d of daily) {
      const t = (d.promptTokens || 0) + (d.completionTokens || 0);
      if (!best || t > (best.promptTokens || 0) + (best.completionTokens || 0)) best = d;
    }
    return best;
  }, [daily]);

  const heatmap = useMemo(() => buildHeatmap(daily), [daily]);

  const activeDays = useMemo(
    () => daily.filter((d) => (d.promptTokens || 0) + (d.completionTokens || 0) > 0).length,
    [daily]
  );

  const ioRatio =
    totals.completion > 0 ? totals.prompt / totals.completion : 0;
  const avgTokens = totals.requests > 0 ? totals.total / totals.requests : 0;
  const costPerReq = totals.requests > 0 ? totals.cost / totals.requests : 0;

  const busiestLabel = useMemo(() => {
    if (!busiest) return "—";
    const dt = new Date(busiest.date + "T00:00:00");
    const dayName = dt.toLocaleString("en-US", { weekday: "long" });
    const mon = dt.toLocaleString("en-US", { month: "short" });
    const dayNum = dt.getDate();
    const tokens = (busiest.promptTokens || 0) + (busiest.completionTokens || 0);
    return `${dayName} ${mon} ${dayNum} · ${formatTokens(tokens)} tokens`;
  }, [busiest]);

  return (
    <div className="flex min-w-0 flex-col gap-6">
      {/* Top bar */}
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex items-center gap-2">
          <span className="material-symbols-outlined text-[22px] text-text">insights</span>
          <h2 className="text-lg font-semibold text-text">Usage Analytics</h2>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <button className="flex items-center gap-1.5 rounded-mini-md border border-border bg-surface-2 px-3 py-1.5 text-xs font-medium text-text-muted hover:text-text">
            <span className="material-symbols-outlined text-[16px]">key</span>
            All Keys
            <span className="material-symbols-outlined text-[16px]">expand_more</span>
          </button>
          <div className="flex items-center gap-1">
            {ANALYTICS_PERIODS.map((p) => {
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
            <span className="material-symbols-outlined text-[16px] text-text-muted">calendar_today</span>
          </div>
        </div>
      </div>

      {loading ? (
        <div className="flex items-center justify-center py-16 text-text-muted">
          <span className="material-symbols-outlined animate-spin text-3xl">progress_activity</span>
        </div>
      ) : (
        <>
          {/* KPI cards */}
          <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-4">
            <KpiCard
              label="Total Tokens"
              value={formatTokens(totals.total)}
              sub={`${formatNumber(totals.requests)} Requests`}
              color={USAGE_COLORS.total}
              icon="toll"
            />
            <KpiCard
              label="Input Tokens"
              value={formatTokens(totals.prompt)}
              color={USAGE_COLORS.input}
              icon="input"
            />
            <KpiCard
              label="Output Tokens"
              value={formatTokens(totals.completion)}
              color={USAGE_COLORS.output}
              icon="output"
            />
            <KpiCard
              label="Est Cost"
              value={formatCost(totals.cost)}
              color={USAGE_COLORS.cost}
              icon="attach_money"
            />
          </div>

          {/* Infrastructure + Performance + Highlights */}
          <div className="grid grid-cols-1 gap-4 lg:grid-cols-3">
            <div className="rounded-mini-xl border border-border bg-surface-2 p-5">
              <SectionTitle>Infrastructure</SectionTitle>
              <div className="grid grid-cols-2 gap-4">
                <StatCell label="Accounts" value={formatNumber(infra.accounts)} />
                <StatCell label="Providers" value={formatNumber(infra.providers)} link color={USAGE_COLORS.violet} />
                <StatCell label="API Keys" value={formatNumber(infra.keys)} />
                <StatCell label="Models" value={formatNumber(infra.models)} />
              </div>
            </div>
            <div className="rounded-mini-xl border border-border bg-surface-2 p-5">
              <SectionTitle>Performance</SectionTitle>
              <div className="grid grid-cols-2 gap-4">
                <StatCell label="Avg Tokens/Req" value={formatTokens(avgTokens)} color={USAGE_COLORS.cyan} />
                <StatCell label="Cost/Req" value={formatCost(costPerReq)} color={USAGE_COLORS.cost} />
                <StatCell label="I/O Ratio" value={`${ioRatio.toFixed(1)}x`} color={USAGE_COLORS.violet} />
                <StatCell label="Fast Requests" value={formatNumber(0)} />
              </div>
            </div>
            <div className="rounded-mini-xl border border-border bg-surface-2 p-5">
              <SectionTitle>Highlights</SectionTitle>
              <div className="grid grid-cols-1 gap-3">
                <StatCell
                  label="Top Model"
                  value={topModel ? topModel.name : "—"}
                  color={USAGE_COLORS.pink}
                />
                <StatCell
                  label="Top Provider"
                  value={topProvider ? providerName(topProvider[0]) : "—"}
                  color={USAGE_COLORS.teal}
                />
                <StatCell label="Busiest Day" value={busiestLabel} color={USAGE_COLORS.pink} />
                <div className="grid grid-cols-2 gap-3">
                  <StatCell label="Diversity" value={formatPercent(diversity)} color={USAGE_COLORS.cyan} />
                  <StatCell label="Fallback Rate" value="0.0%" color={USAGE_COLORS.cost} />
                </div>
              </div>
            </div>
          </div>

          {/* Overview heatmap + Most active day */}
          <div className="grid grid-cols-1 gap-4 lg:grid-cols-[minmax(0,2fr)_minmax(0,1fr)]">
            <div className="rounded-mini-xl border border-border bg-surface-2 p-5">
              <SectionTitle>Overview</SectionTitle>
              <div className="mb-3 text-xs text-text-muted">
                {activeDays} active days · {formatTokens(totals.total)} tokens · 365 days
              </div>
              <div className="overflow-x-auto">
                <div className="inline-block">
                  <div className="mb-1 flex gap-[3px] pl-7 text-[10px] text-text-muted">
                    {heatmap.monthLabels.map((m, i) => (
                      <span key={i} style={{ minWidth: 0 }} className="shrink-0">
                        {m.label}
                      </span>
                    ))}
                  </div>
                  <div className="flex gap-[3px]">
                    {heatmap.weeks.map((week, wi) => (
                      <div key={wi} className="flex flex-col gap-[3px]">
                        {week.map((cell, di) => (
                          <div
                            key={di}
                            title={`${cell.date}: ${formatTokens(cell.tokens)} tokens`}
                            className="h-3 w-3 rounded-[2px]"
                            style={{
                              backgroundColor: heatColor(cell.tokens, heatmap.max),
                              opacity: cell.inRange ? 1 : 0.25,
                            }}
                          />
                        ))}
                      </div>
                    ))}
                  </div>
                </div>
              </div>
            </div>
            <div className="rounded-mini-xl border border-border bg-surface-2 p-5">
              <SectionTitle>Most Active Day</SectionTitle>
              <div className="flex h-full flex-col justify-center gap-1">
                <span className="text-xl font-bold text-text">{busiestLabel}</span>
                <span className="text-xs text-text-muted">
                  Peak daily token volume in the selected range
                </span>
              </div>
            </div>
          </div>
        </>
      )}
    </div>
  );
}
