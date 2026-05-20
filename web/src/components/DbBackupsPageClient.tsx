"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { Button, Card } from "@/shared/components";

// ──────────────────────────────────────────────────────────────────────
// Types — mirror src/db/backups.rs and src/server/api/db_backups.rs.
// ──────────────────────────────────────────────────────────────────────

interface BackupInfo {
  id: string;
  filename: string;
  createdAt: string;
  size: number;
  reason: string;
  providerCount: number;
  comboCount: number;
  apiKeyCount: number;
}

interface ListResponse {
  backups: BackupInfo[];
  maxFiles: number;
  retentionDays: number;
  autoDisabled: boolean;
}

type StatusMessage = { type: "success" | "error" | "info"; text: string };

const REASON_LABEL: Record<string, string> = {
  auto: "Auto",
  manual: "Manual",
  "pre-restore": "Pre-restore",
  "pre-import": "Pre-import",
};

function StatusAlert({ status }: { status: StatusMessage | null }) {
  if (!status) return null;
  const cls =
    status.type === "success"
      ? "border-green-300 bg-green-50 text-green-800 dark:border-green-700 dark:bg-green-900/30 dark:text-green-200"
      : status.type === "error"
      ? "border-red-300 bg-red-50 text-red-800 dark:border-red-700 dark:bg-red-900/30 dark:text-red-200"
      : "border-blue-300 bg-blue-50 text-blue-800 dark:border-blue-700 dark:bg-blue-900/30 dark:text-blue-200";
  return (
    <div className={`mt-3 rounded-md border px-3 py-2 text-sm ${cls}`}>{status.text}</div>
  );
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
}

function formatTime(iso: string): string {
  try {
    return new Date(iso).toLocaleString();
  } catch {
    return iso;
  }
}

export default function DbBackupsPageClient() {
  const [data, setData] = useState<ListResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState<StatusMessage | null>(null);
  const [pendingRestore, setPendingRestore] = useState<BackupInfo | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const fetchList = useCallback(async () => {
    setLoading(true);
    try {
      const res = await fetch("/api/db-backups");
      if (!res.ok) throw new Error(`Server returned ${res.status}`);
      const json = (await res.json()) as ListResponse;
      setData(json);
    } catch (err) {
      setStatus({
        type: "error",
        text: err instanceof Error ? `Failed to load backups: ${err.message}` : "Failed to load backups",
      });
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void fetchList();
  }, [fetchList]);

  const handleCreate = useCallback(async () => {
    setBusy(true);
    setStatus(null);
    try {
      const res = await fetch("/api/db-backups", { method: "PUT" });
      if (!res.ok) throw new Error(`Server returned ${res.status}`);
      const json = await res.json();
      if (json?.created === false) {
        setStatus({ type: "info", text: json?.message || "Backup skipped." });
      } else {
        setStatus({ type: "success", text: `Snapshot created: ${json?.backup?.id ?? "(no id)"}` });
      }
      await fetchList();
    } catch (err) {
      setStatus({
        type: "error",
        text: err instanceof Error ? `Failed to create backup: ${err.message}` : "Failed to create backup",
      });
    } finally {
      setBusy(false);
    }
  }, [fetchList]);

  const handleDelete = useCallback(
    async (id: string) => {
      if (!confirm(`Delete backup ${id}?`)) return;
      setBusy(true);
      setStatus(null);
      try {
        const res = await fetch(`/api/db-backups/${encodeURIComponent(id)}`, { method: "DELETE" });
        if (!res.ok) throw new Error(`Server returned ${res.status}`);
        setStatus({ type: "success", text: "Backup deleted." });
        await fetchList();
      } catch (err) {
        setStatus({
          type: "error",
          text: err instanceof Error ? `Failed to delete: ${err.message}` : "Failed to delete",
        });
      } finally {
        setBusy(false);
      }
    },
    [fetchList]
  );

  const handleCleanup = useCallback(async () => {
    if (!confirm("Prune backups using the current retention settings?")) return;
    setBusy(true);
    setStatus(null);
    try {
      const res = await fetch("/api/db-backups", {
        method: "DELETE",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({}),
      });
      if (!res.ok) throw new Error(`Server returned ${res.status}`);
      const json = await res.json();
      const r = json?.result;
      setStatus({
        type: "success",
        text: `Cleanup ran (deleted ${r?.deletedFiles ?? 0}, kept ${r?.keptFiles ?? 0}).`,
      });
      await fetchList();
    } catch (err) {
      setStatus({
        type: "error",
        text: err instanceof Error ? `Cleanup failed: ${err.message}` : "Cleanup failed",
      });
    } finally {
      setBusy(false);
    }
  }, [fetchList]);

  const confirmRestore = useCallback(async () => {
    const target = pendingRestore;
    if (!target) return;
    setPendingRestore(null);
    setBusy(true);
    setStatus(null);
    try {
      const res = await fetch("/api/db-backups/restore", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ backupId: target.id }),
      });
      if (!res.ok) {
        const text = await res.text();
        throw new Error(text || `Server returned ${res.status}`);
      }
      const json = await res.json();
      setStatus({
        type: "success",
        text: `Restored ${target.id} — ${json?.providerCount ?? 0} providers, ${
          json?.comboCount ?? 0
        } combos, ${json?.apiKeyCount ?? 0} API keys.`,
      });
      await fetchList();
    } catch (err) {
      setStatus({
        type: "error",
        text: err instanceof Error ? `Restore failed: ${err.message}` : "Restore failed",
      });
    } finally {
      setBusy(false);
    }
  }, [pendingRestore, fetchList]);

  const handleExport = useCallback(() => {
    // Browser handles the download via the response's Content-Disposition.
    window.location.href = "/api/db-backups/export";
  }, []);

  const handleImportClick = useCallback(() => {
    fileInputRef.current?.click();
  }, []);

  const handleImportFile = useCallback(
    async (event: React.ChangeEvent<HTMLInputElement>) => {
      const file = event.target.files?.[0];
      event.target.value = "";
      if (!file) return;
      if (
        !confirm(
          `Import ${file.name}? This replaces the current database. A pre-import snapshot will be created automatically.`
        )
      ) {
        return;
      }
      setBusy(true);
      setStatus(null);
      try {
        const form = new FormData();
        form.append("file", file);
        const res = await fetch("/api/db-backups/import", { method: "POST", body: form });
        if (!res.ok) {
          const text = await res.text();
          throw new Error(text || `Server returned ${res.status}`);
        }
        const json = await res.json();
        setStatus({
          type: "success",
          text: `Imported ${file.name} — ${json?.providerCount ?? 0} providers, ${
            json?.comboCount ?? 0
          } combos, ${json?.apiKeyCount ?? 0} API keys.`,
        });
        await fetchList();
      } catch (err) {
        setStatus({
          type: "error",
          text: err instanceof Error ? `Import failed: ${err.message}` : "Import failed",
        });
      } finally {
        setBusy(false);
      }
    },
    [fetchList]
  );

  const backups = data?.backups ?? [];

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-semibold text-ink">Database Backups</h1>
        <p className="text-sm text-body mt-1">
          Hourly snapshots of <code>db.json</code> with retention. Restore, export,
          and import full databases here.
        </p>
      </div>

      <Card>
        <div className="p-4 flex flex-wrap items-center gap-2 justify-between">
          <div className="text-sm text-body">
            {data ? (
              <>
                <span className="font-medium text-ink">{backups.length}</span> snapshot
                {backups.length === 1 ? "" : "s"} · retention: max{" "}
                <span className="font-medium text-ink">{data.maxFiles}</span> files
                {data.retentionDays > 0 ? `, ${data.retentionDays} days` : ", no day cutoff"}
                {data.autoDisabled ? " · auto-backup DISABLED" : ""}
              </>
            ) : (
              "Loading…"
            )}
          </div>
          <div className="flex flex-wrap gap-2">
            <Button variant="secondary" onClick={() => void fetchList()} disabled={loading || busy}>
              Refresh
            </Button>
            <Button onClick={() => void handleCreate()} disabled={busy}>
              {busy ? "Working…" : "Create snapshot"}
            </Button>
            <Button variant="secondary" onClick={() => void handleCleanup()} disabled={busy}>
              Prune
            </Button>
            <Button variant="secondary" onClick={handleExport} disabled={busy}>
              Export db.json
            </Button>
            <Button variant="secondary" onClick={handleImportClick} disabled={busy}>
              Import db.json
            </Button>
            <input
              ref={fileInputRef}
              type="file"
              accept=".json,application/json"
              className="hidden"
              onChange={handleImportFile}
            />
          </div>
        </div>
        <StatusAlert status={status} />
      </Card>

      <Card>
        <div className="overflow-x-auto">
          <table className="min-w-full text-sm">
            <thead className="text-left text-body uppercase text-xs tracking-wide">
              <tr>
                <th className="px-4 py-2">Snapshot</th>
                <th className="px-4 py-2">Reason</th>
                <th className="px-4 py-2">Created</th>
                <th className="px-4 py-2">Size</th>
                <th className="px-4 py-2">Providers</th>
                <th className="px-4 py-2">Combos</th>
                <th className="px-4 py-2">API keys</th>
                <th className="px-4 py-2 text-right">Actions</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-line">
              {loading && (
                <tr>
                  <td colSpan={8} className="px-4 py-6 text-center text-body">
                    Loading backups…
                  </td>
                </tr>
              )}
              {!loading && backups.length === 0 && (
                <tr>
                  <td colSpan={8} className="px-4 py-6 text-center text-body">
                    No backups yet. Click <span className="font-medium">Create snapshot</span> to
                    save one now, or wait for the hourly auto-backup.
                  </td>
                </tr>
              )}
              {!loading &&
                backups.map((b) => (
                  <tr key={b.id} className="hover:bg-surface-soft">
                    <td className="px-4 py-2 font-mono text-xs text-ink">{b.id}</td>
                    <td className="px-4 py-2 text-body">
                      {REASON_LABEL[b.reason] || b.reason}
                    </td>
                    <td className="px-4 py-2 text-body">{formatTime(b.createdAt)}</td>
                    <td className="px-4 py-2 text-body">{formatSize(b.size)}</td>
                    <td className="px-4 py-2 text-body">{b.providerCount}</td>
                    <td className="px-4 py-2 text-body">{b.comboCount}</td>
                    <td className="px-4 py-2 text-body">{b.apiKeyCount}</td>
                    <td className="px-4 py-2 text-right">
                      <div className="inline-flex gap-2">
                        <Button
                          variant="secondary"
                          onClick={() => setPendingRestore(b)}
                          disabled={busy}
                        >
                          Restore
                        </Button>
                        <Button
                          variant="secondary"
                          onClick={() => void handleDelete(b.id)}
                          disabled={busy}
                        >
                          Delete
                        </Button>
                      </div>
                    </td>
                  </tr>
                ))}
            </tbody>
          </table>
        </div>
      </Card>

      {pendingRestore && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/40"
          onClick={() => setPendingRestore(null)}
        >
          <div
            className="max-w-md rounded-lg bg-surface-card p-6 shadow-xl"
            onClick={(e) => e.stopPropagation()}
          >
            <h2 className="text-lg font-semibold text-ink">Restore snapshot?</h2>
            <p className="mt-2 text-sm text-body">
              This will replace the current database with the contents of{" "}
              <span className="font-mono">{pendingRestore.id}</span>. A pre-restore safety
              snapshot will be created first.
            </p>
            <p className="mt-2 text-sm text-body">
              {pendingRestore.providerCount} providers · {pendingRestore.comboCount} combos ·{" "}
              {pendingRestore.apiKeyCount} API keys.
            </p>
            <div className="mt-4 flex justify-end gap-2">
              <Button variant="secondary" onClick={() => setPendingRestore(null)}>
                Cancel
              </Button>
              <Button onClick={() => void confirmRestore()} disabled={busy}>
                Restore
              </Button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
