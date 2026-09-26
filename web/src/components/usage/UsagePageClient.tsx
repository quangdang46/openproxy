import { Suspense, useState, useEffect, useMemo } from "react";
// import { useSearchParams, useRouter } from "next/navigation";  // ported: next.js -> Astro+React
import { UsageStats, RequestLogger, CardSkeleton, SegmentedControl } from "@/shared/components";
import RequestDetailsTab from "@/components/usage/RequestDetailsTab";
import UsageAnalyticsGrid from "@/components/usage/UsageAnalyticsGrid";
import CompressionStats from "@/components/usage/CompressionStats";
import type { UsageRouter } from "@/shared/components/UsageStats";

const PERIODS = [
  { value: "today", label: "Today" },
  { value: "24h", label: "24h" },
  { value: "7d", label: "7D" },
  { value: "30d", label: "30D" },
  { value: "60d", label: "60D" },
];

// 9router's `?tab=` contract is exactly these three (page.js:31-33); a link
// carrying anything else falls back to the overview in both products. The
// control offers two of them — `logs` is reachable by deep link only.
export const USAGE_TABS = ["overview", "logs", "details"] as const;
export type UsageTab = (typeof USAGE_TABS)[number];

export function resolveUsageTab(value: string | null): UsageTab {
  return USAGE_TABS.includes(value as UsageTab) ? (value as UsageTab) : "overview";
}

export default function UsagePageClient() {
  return (
    <Suspense fallback={<CardSkeleton />}>
      <UsageContent />
    </Suspense>
  );
}

function UsageContent() {
  // useSearchParams/useRouter shims for Astro (no Next.js runtime)
  const [searchParams, setSearchParams] = useState<URLSearchParams>(
    () => new URLSearchParams(typeof window !== "undefined" ? window.location.search : "")
  );
  useEffect(() => {
    const onPopState = () => setSearchParams(new URLSearchParams(window.location.search));
    window.addEventListener("popstate", onPopState);
    return () => window.removeEventListener("popstate", onPopState);
  }, []);
  // This page owns the query string, so children write through this shim
  // instead of touching history themselves. Memoized so the identity survives
  // re-renders — a fresh object per render would churn their callbacks.
  const router = useMemo<UsageRouter & { push: (url: string, _opts?: { scroll?: boolean }) => void }>(() => ({
    push: (url: string, _opts?: { scroll?: boolean }) => {
      window.history.pushState(null, "", url);
      setSearchParams(new URLSearchParams(window.location.search));
    },
    replace: (url: string) => {
      window.history.replaceState(null, "", url);
      setSearchParams(new URLSearchParams(window.location.search));
    },
  }), []);

  const [tabLoading, setTabLoading] = useState(false);
  const [period, setPeriod] = useState("today");

  const tabFromUrl = searchParams.get("tab");
  const activeTab = resolveUsageTab(tabFromUrl);

  const handleTabChange = (value: string) => {
    if (value === activeTab) return;
    setTabLoading(true);
    const params = new URLSearchParams(searchParams);
    params.set("tab", value);
    router.push(`/dashboard/usage?${params.toString()}`, { scroll: false });
    setTimeout(() => setTabLoading(false), 300);
  };

  return (
    <div className="flex min-w-0 flex-col gap-6 px-1 sm:px-0">
      {/* Tabs + period selector on same row */}
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <SegmentedControl
          options={[
            { value: "overview", label: "Overview" },
            { value: "details", label: "Details" },
          ]}
          value={activeTab}
          onChange={handleTabChange}
          className="w-full sm:w-auto"
        />
        {activeTab === "overview" && (
          <SegmentedControl
            options={PERIODS}
            value={period}
            onChange={setPeriod}
            size="sm"
            className="w-full sm:w-auto"
          />
        )}
      </div>

      {tabLoading ? (
        <CardSkeleton />
      ) : (
        <>
          {activeTab === "overview" && (
            <Suspense fallback={<CardSkeleton />}>
              <UsageStats period={period} setPeriod={setPeriod} hidePeriodSelector router={router} />
              {/* OpenProxy's extra read-outs, kept on the overview rather than
                  in the shared ?tab= contract so a `?tab=providers` link still
                  lands where 9router lands it. ProviderBreakdownTable is not
                  among them: it is fed by `byProvider`, which only UsageStats
                  holds, so from here it could only ever render its empty
                  state. */}
              <UsageAnalyticsGrid period={period} />
              <CompressionStats period={period} />
            </Suspense>
          )}
          {activeTab === "logs" && <RequestLogger />}
          {activeTab === "details" && <RequestDetailsTab />}
        </>
      )}
    </div>
  );
}
