"use client";

import Card from "@/shared/components/Card";

interface Stats {
  totalRequests?: number;
  totalPromptTokens?: number;
  totalCompletionTokens?: number;
  totalReasoningTokens?: number;
  totalCost?: number;
}

const compact = new Intl.NumberFormat(undefined, {
  notation: "compact",
  maximumFractionDigits: 1,
});

const fmt = (n: number) => compact.format(n || 0);

interface OverviewCardsProps {
  stats: Stats;
}

export default function OverviewCards({ stats }: OverviewCardsProps) {
  const totalTokens =
    (stats.totalPromptTokens || 0) +
    (stats.totalCompletionTokens || 0) +
    (stats.totalReasoningTokens || 0);

  return (
    <div className="grid min-w-0 grid-cols-1 gap-3 sm:grid-cols-2 md:grid-cols-3">
      <Card className="flex min-w-0 flex-col gap-1 px-4 py-3">
        <span className="text-text-muted text-sm uppercase font-semibold">Total Tokens</span>
        <span className="truncate text-2xl font-bold">{fmt(totalTokens)}</span>
        <span className="text-[11px] text-text-muted">{fmt(stats.totalRequests || 0)} Requests</span>
      </Card>
      <Card className="flex min-w-0 flex-col gap-1 px-4 py-3">
        <span className="text-text-muted text-sm uppercase font-semibold">Input Tokens</span>
        <span className="truncate text-2xl font-bold text-[color:var(--color-danger)]">{fmt(stats.totalPromptTokens || 0)}</span>
      </Card>
      <Card className="flex min-w-0 flex-col gap-1 px-4 py-3">
        <span className="text-text-muted text-sm uppercase font-semibold">Output Tokens</span>
        <span className="truncate text-2xl font-bold text-[color:var(--color-success)]">{fmt(stats.totalCompletionTokens || 0)}</span>
      </Card>
    </div>
  );
}
