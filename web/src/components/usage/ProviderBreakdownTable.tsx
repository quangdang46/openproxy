"use client";

import Card from "@/shared/components/Card";
import { AI_PROVIDERS } from "@/shared/constants/providers";

interface ProviderStat {
  requests?: number;
  promptTokens?: number;
  completionTokens?: number;
  cachedTokens?: number;
  cost?: number;
}

interface ProviderBreakdownTableProps {
  byProvider?: Record<string, ProviderStat>;
}

const compact = new Intl.NumberFormat(undefined, {
  notation: "compact",
  maximumFractionDigits: 1,
});

const fmtInt = (n: number) => new Intl.NumberFormat().format(n || 0);
const fmtCost = (n: number) => `$${(n || 0).toFixed(2)}`;

export default function ProviderBreakdownTable({ byProvider }: ProviderBreakdownTableProps) {
  const entries = Object.entries(byProvider || {}).map(([id, data]) => {
    const config = AI_PROVIDERS[id] || { color: "#6b7280", name: id };
    const input = data.promptTokens || 0;
    const output = data.completionTokens || 0;
    const total = input + output;
    return {
      id,
      name: config.name || id,
      color: config.color || "#6b7280",
      requests: data.requests || 0,
      input,
      output,
      total,
      cost: data.cost || 0,
    };
  });

  // Sort by TOTAL descending
  entries.sort((a, b) => b.total - a.total);

  const grandTotal = entries.reduce((sum, e) => sum + e.total, 0);

  return (
    <Card padding="none" className="overflow-hidden">
      <div className="px-4 py-3 border-b border-border">
        <span className="text-sm font-semibold text-text-muted uppercase tracking-wide">Provider Breakdown</span>
      </div>

      {entries.length === 0 ? (
        <div className="px-4 py-8 text-center text-text-muted text-sm">No provider usage recorded yet.</div>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full min-w-[640px] border-collapse text-sm">
            <thead>
              <tr className="border-b border-border text-text-muted">
                <th className="px-4 py-2.5 text-left font-semibold text-xs uppercase tracking-wide">Provider</th>
                <th className="px-4 py-2.5 text-right font-semibold text-xs uppercase tracking-wide">Requests</th>
                <th className="px-4 py-2.5 text-right font-semibold text-xs uppercase tracking-wide">Input</th>
                <th className="px-4 py-2.5 text-right font-semibold text-xs uppercase tracking-wide">Output</th>
                <th className="px-4 py-2.5 text-right font-semibold text-xs uppercase tracking-wide">Total</th>
                <th className="px-4 py-2.5 text-right font-semibold text-xs uppercase tracking-wide">Cost</th>
                <th className="px-4 py-2.5 text-right font-semibold text-xs uppercase tracking-wide">Share</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-border/60">
              {entries.map((e) => {
                const share = grandTotal > 0 ? (e.total / grandTotal) * 100 : 0;
                return (
                  <tr key={e.id} className="hover:bg-surface-soft transition-colors">
                    <td className="px-4 py-2.5">
                      <div className="flex items-center gap-2 min-w-0">
                        <span
                          className="block w-2 h-2 rounded-full shrink-0"
                          style={{ backgroundColor: e.color }}
                        />
                        <span className="font-medium truncate">{e.name}</span>
                      </div>
                    </td>
                    <td className="px-4 py-2.5 text-right text-text-muted whitespace-nowrap">{fmtInt(e.requests)}</td>
                    <td className="px-4 py-2.5 text-right text-[color:var(--color-danger)] whitespace-nowrap">{compact.format(e.input)}</td>
                    <td className="px-4 py-2.5 text-right text-[color:var(--color-success)] whitespace-nowrap">{compact.format(e.output)}</td>
                    <td className="px-4 py-2.5 text-right font-bold whitespace-nowrap">{compact.format(e.total)}</td>
                    <td className="px-4 py-2.5 text-right text-[color:var(--color-warning)] whitespace-nowrap">{fmtCost(e.cost)}</td>
                    <td className="px-4 py-2.5">
                      <div className="flex items-center justify-end gap-2">
                        <div className="flex-1 max-w-[120px] h-1.5 rounded-full bg-surface-soft overflow-hidden">
                          <div
                            className="h-full rounded-full"
                            style={{ width: `${share}%`, backgroundColor: e.color }}
                          />
                        </div>
                        <span className="text-text-muted text-xs tabular-nums w-10 text-right">{share.toFixed(1)}%</span>
                      </div>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </Card>
  );
}
