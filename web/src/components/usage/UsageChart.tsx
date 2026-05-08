"use client";

import { lazy, Suspense } from "react";
import Card from "@/shared/components/Card";

// Lazy load Recharts components
const ChartComponent = lazy(() => import('./UsageChartInner'));

interface UsageChartProps {
  period?: string;
}

function LoadingFallback() {
  return (
    <Card padding="lg">
      <div className="flex items-center justify-center h-64">
        <span className="material-symbols-outlined animate-spin text-2xl">progress_activity</span>
      </div>
    </Card>
  );
}

export default function UsageChart({ period = "7d" }: UsageChartProps) {
  return (
    <Suspense fallback={<LoadingFallback />}>
      <ChartComponent period={period} />
    </Suspense>
  );
}
