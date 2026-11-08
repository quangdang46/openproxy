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
import { formatTokens, formatNumber } from "./usageFormat";

interface ModelStat {
  requests?: number;
  promptTokens?: number;
  completionTokens?: number;
  cost?: number;
  rawModel?: string;
  provider?: string;
}

interface TopModelsChartProps {
  byModel?: Record<string, ModelStat>;
}

type ViewMode = "tokens" | "requests";

const fmtRequests = (n: number) => formatNumber(n);
const truncate = (s: string, max = 22) => (s && s.length > max ? s.slice(0, max) + "…" : s || "");

const CHART_COLORS = ["#6366f1", "#14b8a6", "#f59e0b", "#ef4444", "#8b5cf6"];

// Port of 9router TopModelsChart.js — top-5 models ranked by tokens or
// requests, fed by the /api/usage/stats byModel map.
export default function TopModelsChart({ byModel }: TopModelsChartProps) {
  const [viewMode, setViewMode] = useState<ViewMode>("tokens");

  const chartData = useMemo(() => {
    if (!byModel) return [];
    return Object.values(byModel)
      .map((data) => ({
        name: truncate(data.rawModel || "Unknown"),
        tokens: (data.promptTokens || 0) + (data.completionTokens || 0),
        requests: data.requests || 0,
      }))
      .filter((d) => d[viewMode] > 0)
      .sort((a, b) => b[viewMode] - a[viewMode])
      .slice(0, 5)
      .map((d, i) => ({ ...d, color: CHART_COLORS[i % CHART_COLORS.length] }));
  }, [byModel, viewMode]);

  const fmt = viewMode === "tokens" ? formatTokens : fmtRequests;
  const label = viewMode === "tokens" ? "Tokens" : "Requests";

  return (
    <Card className="flex min-w-0 flex-col gap-3 p-3 sm:p-4">
      <div className="flex items-center justify-between gap-2">
        <span className="text-sm font-semibold text-text-muted uppercase tracking-wide">Top Models</span>
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
        <div className="h-44 flex items-center justify-center text-text-muted text-sm">No model usage yet</div>
      ) : (
        <ResponsiveContainer width="100%" height={180}>
          <BarChart
            data={chartData}
            layout="vertical"
            margin={{ top: 4, right: 40, left: 4, bottom: 4 }}
          >
            <CartesianGrid strokeDasharray="3 3" strokeOpacity={0.1} horizontal={false} />
            <XAxis
              type="number"
              tick={{ fontSize: 10, fill: "currentColor", fillOpacity: 0.5 }}
              tickLine={false}
              axisLine={false}
              tickFormatter={fmt}
            />
            <YAxis
              type="category"
              dataKey="name"
              tick={{ fontSize: 10, fill: "currentColor", fillOpacity: 0.7 }}
              tickLine={false}
              axisLine={false}
              width={90}
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
            <Bar dataKey={viewMode} radius={[0, 4, 4, 0]}>
              {chartData.map((d, i) => (
                <Cell key={i} fill={d.color} fillOpacity={0.85} />
              ))}
            </Bar>
          </BarChart>
        </ResponsiveContainer>
      )}
    </Card>
  );
}
