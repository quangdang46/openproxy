"use client";

import { lazy, Suspense } from "react";
import PropTypes from "prop-types";
import Card from "@/shared/components/Card";

// Lazy load Recharts components
const ChartComponent = lazy(() => import('./UsageChartInner'));

function LoadingFallback() {
  return (
    <Card padding="lg">
      <div className="flex items-center justify-center h-64">
        <span className="material-symbols-outlined animate-spin text-2xl">progress_activity</span>
      </div>
    </Card>
  );
}

export default function UsageChart({ period = "7d" }) {
  return (
    <Suspense fallback={<LoadingFallback />}>
      <ChartComponent period={period} />
    </Suspense>
  );
}

UsageChart.propTypes = {
  period: PropTypes.string,
};
