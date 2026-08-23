"use client";

import { useEffect, useState } from "react";
import Card from "@/shared/components/Card";

interface CacheStats {
  hits: number;
  misses: number;
  sets: number;
  entries: number;
  hit_rate: number;
}

const fmtPct = (n: number) => `${(n * 100).toFixed(1)}%`;
const fmt = (n: number) => new Intl.NumberFormat().format(n || 0);

/**
 * Response-cache hit-rate card for the dashboard home.
 *
 * Polls GET /api/cache/stats (admin) and shows how often repeated prompts are
 * served from cache instead of hitting a provider — the free-tier quota saver.
 */
export default function CacheStatsCard() {
  const [stats, setStats] = useState<CacheStats | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    const load = async () => {
      try {
        const res = await fetch("/api/cache/stats", { cache: "no-store" });
        if (!res.ok) {
          setError(`HTTP ${res.status}`);
          return;
        }
        const data = (await res.json()) as CacheStats;
        if (alive) {
          setStats(data);
          setError(null);
        }
      } catch (e) {
        if (alive) setError(e instanceof Error ? e.message : "fetch failed");
      }
    };
    load();
    const id = setInterval(load, 5000);
    return () => {
      alive = false;
      clearInterval(id);
    };
  }, []);

  return (
    <Card>
      <h2 className="text-lg font-semibold mb-4 flex items-center gap-2">
        <span className="material-symbols-outlined text-primary">dns</span>
        Response Cache
      </h2>
      {error ? (
        <p className="text-sm text-text-muted">Unavailable: {error}</p>
      ) : !stats ? (
        <p className="text-sm text-text-muted">Loading…</p>
      ) : (
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          <div className="flex flex-col gap-1">
            <span className="text-text-muted text-xs uppercase font-semibold">Hit Rate</span>
            <span className="text-2xl font-bold text-success">{fmtPct(stats.hit_rate)}</span>
          </div>
          <div className="flex flex-col gap-1">
            <span className="text-text-muted text-xs uppercase font-semibold">Hits</span>
            <span className="text-2xl font-bold">{fmt(stats.hits)}</span>
          </div>
          <div className="flex flex-col gap-1">
            <span className="text-text-muted text-xs uppercase font-semibold">Misses</span>
            <span className="text-2xl font-bold">{fmt(stats.misses)}</span>
          </div>
          <div className="flex flex-col gap-1">
            <span className="text-text-muted text-xs uppercase font-semibold">Cached</span>
            <span className="text-2xl font-bold text-info">{fmt(stats.entries)}</span>
          </div>
        </div>
      )}
      <p className="mt-3 text-[11px] text-text-muted">
        Repeated prompts served from cache (24h TTL) save provider quota. Non-streaming only.
      </p>
    </Card>
  );
}
