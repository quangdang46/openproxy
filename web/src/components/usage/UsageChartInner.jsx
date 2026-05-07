"use client";

import { useState, useEffect, useCallback } from "react";
import PropTypes from "prop-types";
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

const fmtTokens = (n) => {
  if (n >= 1000000) return `${(n / 1000000).toFixed(1)}M`;
  if (n >= 1000) return `${(n / 1000).toFixed(1)}K`;
  return String(n || 0);
};

const fmtCost = (n) => `$${(n || 0).toFixed(4)}`;

export default function UsageChartInner({ period = "7d" }) {
  const [data, setData] = useState([]);
  const [loading, setLoading] = useState(true);
  const [viewMode, setViewMode] = useState("tokens");

  const fetchData = useCallback(async () => {
    setLoading(true);
    try {
      const res = await fetch(`/api/usage/chart?period=${period}`);
      if (res.ok) {
        const result = await res.json();
        setData(result.data || []);
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

  if (loading) {
    return (
      <Card padding="lg">
        <div className="flex items-center justify-center h-64">
          <span className="material-symbols-outlined animate-spin text-2xl">progress_activity</span>
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
            dataKey="date"
            tick={{ fill: "#888", fontSize: 12 }}
            stroke="#888"
          />
          <YAxis
            tick={{ fill: "#888", fontSize: 12 }}
            stroke="#888"
            tickFormatter={viewMode === "tokens" ? fmtTokens : fmtCost}
          />
          <Tooltip
            formatter={(value) => (viewMode === "tokens" ? fmtTokens(value) : fmtCost(value))}
          />
          <Legend />
          <Area
            type="monotone"
            dataKey={viewMode === "tokens" ? "tokens" : "cost"}
            stroke="#f97815"
            fill="#f97815"
            fillOpacity={0.3}
          />
        </AreaChart>
      </ResponsiveContainer>
    </Card>
  );
}

UsageChartInner.propTypes = {
  period: PropTypes.string,
};
