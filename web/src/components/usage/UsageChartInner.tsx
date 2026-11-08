"use client";

import { useState, useEffect, useCallback } from "react";
import {
  AreaChart,
  Area,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
  Legend,
} from "recharts";
import Card from "@/shared/components/Card";

interface UsageChartInnerProps {
  period?: string;
}

interface ChartDataPoint {
  label: string;
  tokens?: number;
  requests?: number;
  cost?: number;
}

const fmtTokens = (n: number) => {
  if (n >= 1000000) return `${(n / 1000000).toFixed(1)}M`;
  if (n >= 1000) return `${(n / 1000).toFixed(1)}K`;
  return String(n || 0);
};

const fmtRequests = (n: number) => String(n || 0);

const fmtCost = (n: number) => `$${(n || 0).toFixed(4)}`;

type ViewMode = "tokens" | "requests" | "cost";

// Mirrors 9router UsageChart.js VIEW_CONFIG — each mode picks its own bucket
// field, color, and formatter.
const VIEW_CONFIG: Record<
  ViewMode,
  { dataKey: "tokens" | "requests" | "cost"; color: string; formatter: (n: number) => string }
> = {
  tokens: { dataKey: "tokens", color: "#6366f1", formatter: fmtTokens },
  requests: { dataKey: "requests", color: "#14b8a6", formatter: fmtRequests },
  cost: { dataKey: "cost", color: "#f59e0b", formatter: fmtCost },
};

export default function UsageChartInner({ period = "7d" }: UsageChartInnerProps) {
  const [data, setData] = useState<ChartDataPoint[]>([]);
  const [loading, setLoading] = useState<boolean>(true);
  const [viewMode, setViewMode] = useState<ViewMode>("tokens");

  const fetchData = useCallback(async () => {
    setLoading(true);
    try {
      const res = await fetch(`/api/usage/chart?period=${period}`);
      if (res.ok) {
        // 9router UsageChart.js reads the bucket array straight off the
        // response — there is no `{ data: … }` envelope to unwrap.
        const result = await res.json();
        setData(Array.isArray(result) ? result : []);
      }
    } catch (error) {
      console.error("Failed to fetch usage chart data:", error);
    } finally {
      setLoading(false);
    }
  }, [period]);

  useEffect(() => {
    fetchData();
  }, [fetchData]);

  const cfg = VIEW_CONFIG[viewMode];
  // A period can legitimately return buckets that are all zero; drawing an empty
  // axis there reads as a broken chart, so show the empty state instead.
  const hasData = data.some((d) => (d[cfg.dataKey] ?? 0) > 0);

  if (loading) {
    return (
      <Card padding="lg">
        <div className="flex items-center justify-center h-64">
          <span className="material-symbols-outlined animate-spin text-2xl">progress_activity</span>
        </div>
      </Card>
    );
  }

  if (!hasData) {
    return (
      <Card padding="lg">
        <div className="h-48 flex items-center justify-center text-text-muted text-sm">
          No data for this period
        </div>
      </Card>
    );
  }

  return (
    <Card padding="lg">
      <div className="flex items-center justify-between mb-4">
        <h3 className="font-semibold">Usage Chart</h3>
        <div className="flex gap-2">
          <button
            onClick={() => setViewMode("tokens")}
            className={`px-3 py-1 rounded text-sm font-medium transition-colors ${
              viewMode === "tokens" ? "bg-primary text-white" : "bg-bg-subtle text-text-muted hover:text-text"
            }`}
          >
            Tokens
          </button>
          <button
            onClick={() => setViewMode("requests")}
            className={`px-3 py-1 rounded text-sm font-medium transition-colors ${
              viewMode === "requests" ? "bg-primary text-white" : "bg-bg-subtle text-text-muted hover:text-text"
            }`}
          >
            Requests
          </button>
          <button
            onClick={() => setViewMode("cost")}
            className={`px-3 py-1 rounded text-sm font-medium transition-colors ${
              viewMode === "cost" ? "bg-primary text-white" : "bg-bg-subtle text-text-muted hover:text-text"
            }`}
          >
            Cost
          </button>
        </div>
      </div>

      <ResponsiveContainer width="100%" height={300}>
        <AreaChart data={data}>
          <CartesianGrid strokeDasharray="3 3" className="stroke-border" />
          <XAxis
            dataKey="label"
            tick={{ fill: "#888", fontSize: 12 }}
            stroke="#888"
          />
          <YAxis
            tick={{ fill: "#888", fontSize: 12 }}
            stroke="#888"
            tickFormatter={cfg.formatter}
          />
          <Tooltip
            formatter={(value: number) => [cfg.formatter(value), viewMode === "tokens" ? "Tokens" : viewMode === "requests" ? "Requests" : "Cost"]}
          />
          <Legend />
          <Area
            type="monotone"
            dataKey={cfg.dataKey}
            stroke={cfg.color}
            fill={cfg.color}
            fillOpacity={0.3}
          />
        </AreaChart>
      </ResponsiveContainer>
    </Card>
  );
}
