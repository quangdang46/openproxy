"use client";

import Card from "@/shared/components/Card";

export interface Stats {
  totalRequests?: number;
  totalPromptTokens?: number;
  totalCompletionTokens?: number;
  totalReasoningTokens?: number;
  totalCachedTokens?: number;
  totalCost?: number;
}

const compact = new Intl.NumberFormat(undefined, {
  notation: "compact",
  maximumFractionDigits: 1,
});

const fmt = (n: number) => compact.format(n || 0);

export interface OverviewCard {
  label: string;
  value: string;
  tone?: string;
  sub?: string;
}

/**
 * The overview row. 9router leads with Total Requests and keeps cached tokens
 * and the cost estimate as first-class cards — OpenProxy demoted requests to an
 * 11px caption and had no cached or cost card at all, so a cache hit rate or a
 * spend total could not be read off the page.
 */
export function overviewCards(stats: Stats): OverviewCard[] {
  const totalTokens =
    (stats.totalPromptTokens || 0) +
    (stats.totalCompletionTokens || 0) +
    (stats.totalReasoningTokens || 0);

  return [
    { label: "Total Requests", value: fmt(stats.totalRequests || 0) },
    {
      label: "Total Input Tokens",
      value: fmt(stats.totalPromptTokens || 0),
      tone: "text-[color:var(--color-primary)]",
    },
    {
      label: "Cached Tokens",
      value: fmt(stats.totalCachedTokens || 0),
      tone: "text-[color:var(--color-info)]",
    },
    {
      label: "Output Tokens",
      value: fmt(stats.totalCompletionTokens || 0),
      tone: "text-[color:var(--color-success)]",
    },
    {
      label: "Est. Cost",
      value: `~$${(stats.totalCost || 0).toFixed(2)}`,
      tone: "text-[color:var(--color-warning)]",
      sub: "Estimated, not actual billing",
    },
    { label: "Total Tokens", value: fmt(totalTokens) },
  ];
}

interface OverviewCardsProps {
  stats: Stats;
}

export default function OverviewCards({ stats }: OverviewCardsProps) {
  return (
    <div className="grid min-w-0 grid-cols-1 gap-3 sm:grid-cols-2 md:grid-cols-3 lg:grid-cols-5">
      {overviewCards(stats).map((card) => (
        <Card key={card.label} className="flex min-w-0 flex-col gap-1 px-4 py-3">
          <span className="text-text-muted text-sm uppercase font-semibold">{card.label}</span>
          <span className={`truncate text-2xl font-bold ${card.tone || ""}`}>{card.value}</span>
          {card.sub && <span className="text-[11px] text-text-muted">{card.sub}</span>}
        </Card>
      ))}
    </div>
  );
}
