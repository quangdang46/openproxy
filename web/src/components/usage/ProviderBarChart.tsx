"use client";

import { useState, useMemo } from "react";
import {
  BarChart,
  Bar,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
  Cell,
} from "recharts";
import Card from "@/shared/components/Card";
import { formatTokens, formatNumber, providerName, providerColor } from "./usageFormat";

interface ProviderStat {
  requests?: number;
  promptTokens?: number;
  completionTokens?: number;
  cost?: number;
}

interface ProviderBarChartProps {
  byProvider?: Record<string, ProviderStat>;
}

type ViewMode = "tokens" | "requests";

const fmtRequests = (n: number) => formatNumber(n);

// Port of 9router ProviderBarChart.js — horizontal ranking of providers by
// tokens or requests, fed by the /api/usage/stats byProvider map.
export default function ProviderBarChart({ byProvider }: ProviderBarChartProps) {
  const [viewMode, setViewMode] = useState<ViewMode>("tokens");

  const chartData = useMemo(() => {
    if (!byProvider) return [];
    return Object.entries(byProvider)
      .map(([id, data], index) => ({
        id,
        name: providerName(id),
        tokens: (data.promptTokens || 0) + (data.completionTokens || 0),
        requests: data.requests || 0,
        color: providerColor(id, index),
      }))
      .filter((d) => d[viewMode] > 0)
      .sort((a, b) => b[viewMode] - a[viewMode]);
  }, [byProvider, viewMode]);

  const fmt = viewMode === "tokens" ? formatTokens : fmtRequests;
  const label = viewMode === "tokens" ? "Tokens" : "Requests";

  return (
    <Card className="flex min-w-0 flex-col gap-3 p-3 sm:p-4">
      <div className="flex items-center justify-between gap-2">
        <span className="text-sm font-semibold text-text-muted uppercase tracking-wide">By Provider</span>
        <div className="grid grid-cols-2 items-center gap-1 rounded-lg border border-border bg-bg-subtle p-1">
          <button
            onClick={() => setViewMode("tokens")}
            className={`px-2.5 py-0.5 rounded-md text-xs font-medium transition-colors ${viewMode === "tokens" ? "bg-primary text-white shadow-sm" : "text-text-muted hover:text-text hover:bg-bg-hover"}`}
          >
            Tokens
          </button>
          <button
            onClick={() => setViewMode("requests")}
            className={`px-2.5 py-0.5 rounded-md text-xs font-medium transition-colors ${viewMode === "requests" ? "bg-primary text-white shadow-sm" : "text-text-muted hover:text-text hover:bg-bg-hover"}`}
          >
            Requests
          </button>
        </div>
      </div>

      {!chartData.length ? (
        <div className="h-44 flex items-center justify-center text-text-muted text-sm">No provider usage yet</div>
      ) : (
        <ResponsiveContainer width="100%" height={180}>
          <BarChart data={chartData} margin={{ top: 4, right: 8, left: 0, bottom: 4 }}>
            <CartesianGrid strokeDasharray="3 3" strokeOpacity={0.1} vertical={false} />
            <XAxis
              dataKey="name"
              tick={{ fontSize: 10, fill: "currentColor", fillOpacity: 0.6 }}
              tickLine={false}
              axisLine={false}
              interval={0}
              tickFormatter={(v) => (v.length > 10 ? v.slice(0, 10) + "…" : v)}
            />
            <YAxis
              tick={{ fontSize: 10, fill: "currentColor", fillOpacity: 0.5 }}
              tickLine={false}
              axisLine={false}
              tickFormatter={fmt}
              width={44}
            />
            <Tooltip
              contentStyle={{
                backgroundColor: "var(--color-bg)",
                border: "1px solid var(--color-border)",
                borderRadius: "8px",
                fontSize: "12px",
              }}
              formatter={(value: number) => [fmt(value), label]}
            />
            <Bar dataKey={viewMode} radius={[4, 4, 0, 0]}>
              {chartData.map((d) => (
                <Cell key={d.id} fill={d.color} fillOpacity={0.85} />
              ))}
            </Bar>
          </BarChart>
        </ResponsiveContainer>
      )}
    </Card>
  );
}
