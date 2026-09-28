import { useCallback, useEffect, useState } from "react";
import { Button, Card } from "@/shared/components";

/**
 * Catalog sync status + a manual trigger.
 *
 * The backend exposes GET/POST /api/models/catalog-sync so an operator can read
 * the sync timer state and force a run, but the dashboard had no surface for
 * either: the model list simply came from the already-merged /api/catalog, so a
 * failed or never-run sync showed up as a stale model list with no explanation
 * and no way to retry. This mounts that operator surface on the Providers page,
 * which is where someone goes when the model list looks wrong.
 */

type SyncState = {
  lastSync?: string | null;
  lastError?: string | null;
  syncing?: boolean;
  intervalSeconds?: number;
  catalog?: { providers?: number; models?: number; updatedAt?: string } | null;
};

const fmtWhen = (value) => {
  if (!value) return "never";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return String(value);
  return date.toLocaleString();
};

export default function CatalogSyncCard() {
  const [state, setState] = useState<SyncState | null>(null);
  const [loading, setLoading] = useState(true);
  const [syncing, setSyncing] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const res = await fetch("/api/models/catalog-sync", { cache: "no-store" });
      if (!res.ok) {
        setError(`Could not read sync status (HTTP ${res.status}).`);
        return;
      }
      setState(await res.json());
      setError(null);
    } catch (e) {
      setError("Could not reach the server to read sync status.");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const syncNow = async () => {
    setSyncing(true);
    setError(null);
    try {
      const res = await fetch("/api/models/catalog-sync", { method: "POST" });
      const body = await res.json().catch(() => ({}));
      if (!res.ok) {
        // The handler deliberately answers 503 with the last error rather than
        // 200 + success:false, so a 200 here really did sync.
        setError(body?.error || `Sync failed (HTTP ${res.status}).`);
      }
      await load();
    } catch {
      setError("Could not reach the server to start a sync.");
    } finally {
      setSyncing(false);
    }
  };

  const failed = Boolean(state?.lastError);

  return (
    <Card padding="sm" className="min-w-0">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <h3 className="text-sm font-medium text-text">Model catalog sync</h3>
          {loading ? (
            <p className="mt-1 text-xs text-text-muted">Reading sync status…</p>
          ) : (
            <p className="mt-1 text-xs text-text-muted">
              Last synced {fmtWhen(state?.lastSync)}
              {state?.intervalSeconds
                ? ` · every ${Math.round(state.intervalSeconds / 60)} min`
                : ""}
              {state?.catalog?.models != null
                ? ` · ${state.catalog.models} models from ${state.catalog.providers ?? "?"} providers`
                : ""}
            </p>
          )}
          {failed && (
            <p className="mt-1 text-xs text-red-600 dark:text-red-400">
              Last sync failed: {state?.lastError}
            </p>
          )}
          {error && !failed && (
            <p className="mt-1 text-xs text-red-600 dark:text-red-400">{error}</p>
          )}
        </div>
        <div className="flex shrink-0 gap-2">
          <Button
            size="sm"
            variant="ghost"
            onClick={load}
            disabled={loading || syncing}
            title="Re-read sync status"
          >
            Refresh
          </Button>
          <Button size="sm" onClick={syncNow} disabled={syncing || loading}>
            {syncing ? "Syncing…" : "Sync now"}
          </Button>
        </div>
      </div>
    </Card>
  );
}
