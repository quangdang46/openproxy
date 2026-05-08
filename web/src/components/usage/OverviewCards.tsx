"use client";

import Card from "@/shared/components/Card";

interface Stats {
  totalRequests?: number;
  totalPromptTokens?: number;
  totalCompletionTokens?: number;
  totalCost?: number;
}

const fmt = (n: number) => new Intl.NumberFormat().format(n || 0);
const fmtCost = (n: number) => `$${(n || 0).toFixed(2)}`;

interface OverviewCardsProps {
  stats: Stats;
}

export default function OverviewCards({ stats }: OverviewCardsProps) {
  return (
    <div className="grid min-w-0 grid-cols-1 gap-3 sm:grid-cols-2 md:grid-cols-4 sm:gap-4">
      <Card className="flex min-w-0 flex-col gap-1 px-4 py-3">
        <span className="text-text-muted text-sm uppercase font-semibold">Total Requests</span>
        <span className="truncate text-2xl font-bold">{fmt(stats.totalRequests || 0)}</span>
      </Card>
      <Card className="flex min-w-0 flex-col gap-1 px-4 py-3">
        <span className="text-text-muted text-sm uppercase font-semibold">Total Input Tokens</span>
        <span className="truncate text-2xl font-bold text-primary">{fmt(stats.totalPromptTokens || 0)}</span>
      </Card>
      <Card className="flex min-w-0 flex-col gap-1 px-4 py-3">
        <span className="text-text-muted text-sm uppercase font-semibold">Output Tokens</span>
        <span className="truncate text-2xl font-bold text-success">{fmt(stats.totalCompletionTokens || 0)}</span>
      </Card>
      <Card className="flex min-w-0 flex-col gap-1 px-4 py-3">
        <span className="text-text-muted text-sm uppercase font-semibold">Est. Cost</span>
        <span className="truncate text-2xl font-bold text-warning">~{fmtCost(stats.totalCost || 0)}</span>
        <span className="text-[10px] text-text-muted">Estimated, not actual billing</span>
      </Card>
    </div>
  );
}
