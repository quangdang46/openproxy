import { useCallback, useEffect, useState } from "react";
import { Badge, Button, Card } from "@/shared/components";
import { useNotificationStore } from "@/store/notificationStore";

/**
 * Per-provider custom header overrides — port of 9router's
 * `CustomConfigCard.js` (providers/[id]) backed by
 * GET/PUT /api/providers/{id}/overrides.
 *
 * The card pre-fills the registry's built-in headers (`builtinHeaders`),
 * diffs on save so ONLY user-changed rows are stored in
 * `settings.providerOverrides`, and the executor merges them last at
 * dispatch. The blocked-header list mirrors the server-side gate — the
 * client check is UX only.
 */

// Mirrors the server-side gate in /api/providers/{id}/overrides.
const BLOCKED_HEADERS = [
  "host",
  "content-length",
  "content-type",
  "connection",
  "transfer-encoding",
  "authorization",
  "cookie",
];
const HEADER_NAME_RE = /^[A-Za-z0-9-]+$/;
const MAX_HEADERS = 20;

type HeaderRow = { name: string; value: string };

type OverridesResponse = {
  headers?: Record<string, string>;
  builtinHeaders?: Record<string, string>;
};

export default function CustomHeadersCard({ providerId }: { providerId: string }) {
  const notify = useNotificationStore();
  const [expanded, setExpanded] = useState(false);
  const [rows, setRows] = useState<HeaderRow[]>([{ name: "", value: "" }]);
  const [builtin, setBuiltin] = useState<Record<string, string>>({});
  const [hasOverride, setHasOverride] = useState(false);
  const [saving, setSaving] = useState(false);
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    let cancelled = false;
    fetch(`/api/providers/${providerId}/overrides`, { cache: "no-store" })
      .then((r) => (r.ok ? r.json() : null))
      .then((data: OverridesResponse | null) => {
        if (cancelled || !data) return;
        // Effective set = registry built-ins with user overrides layered on top.
        const builtinHeaders = data.builtinHeaders || {};
        const effective = { ...builtinHeaders, ...(data.headers || {}) };
        const headerRows = Object.entries(effective).map(([name, value]) => ({
          name,
          value,
        }));
        setBuiltin(builtinHeaders);
        setRows(headerRows.length ? headerRows : [{ name: "", value: "" }]);
        setHasOverride(Object.keys(data.headers || {}).length > 0);
        setLoaded(true);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [providerId]);

  const setRow = (i: number, field: "name" | "value", value: string) => {
    setRows((prev) => prev.map((r, idx) => (idx === i ? { ...r, [field]: value } : r)));
  };

  const addRow = () => setRows((prev) => [...prev, { name: "", value: "" }]);
  const removeRow = (i: number) =>
    setRows((prev) => (prev.length > 1 ? prev.filter((_, idx) => idx !== i) : prev));

  const save = useCallback(async () => {
    // Diff-on-save: only rows differing from the registry default become
    // overrides, so a code-side registry bump still wins for everything the
    // user left alone.
    const headers: Record<string, string> = {};
    const named = rows.filter((r) => r.name.trim());
    if (named.length > MAX_HEADERS) {
      notify.error(`Too many headers (max ${MAX_HEADERS})`);
      return;
    }
    for (const r of named) {
      const name = r.name.trim();
      if (!HEADER_NAME_RE.test(name)) {
        notify.error(`Invalid header name: ${name}`);
        return;
      }
      if (BLOCKED_HEADERS.includes(name.toLowerCase())) {
        notify.error(`Header ${name} cannot be overridden`);
        return;
      }
      if (name in headers) {
        notify.error(`Duplicate header name: ${name}`);
        return;
      }
      if (r.value !== builtin[name]) headers[name] = r.value;
    }

    setSaving(true);
    try {
      const res = await fetch(`/api/providers/${providerId}/overrides`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ headers }),
      });
      if (!res.ok) {
        const err = await res.json().catch(() => ({}));
        notify.error(err.error || "Failed to save");
        return;
      }
      setHasOverride(Object.keys(headers).length > 0);
      notify.success("Custom headers saved");
    } finally {
      setSaving(false);
    }
  }, [rows, builtin, providerId, notify]);

  const resetToBuiltin = () => {
    setRows(
      Object.entries(builtin).map(([name, value]) => ({ name, value }))
    );
  };

  // Only render when there is something to customize: registry headers or
  // existing overrides.
  if (!loaded || (Object.keys(builtin).length === 0 && !hasOverride)) return null;

  return (
    <Card padding="xs">
      <button
        type="button"
        onClick={() => setExpanded((v) => !v)}
        className="flex w-full items-center justify-between text-left"
      >
        <div className="flex items-center gap-2">
          <span className="material-symbols-outlined text-primary text-[20px]">tune</span>
          <span className="text-sm font-semibold">Custom Headers</span>
          {hasOverride && <Badge variant="success" size="sm">Active</Badge>}
        </div>
        <span className="material-symbols-outlined text-text-muted">
          {expanded ? "expand_less" : "expand_more"}
        </span>
      </button>

      {expanded && (
        <div className="mt-3 border-t border-border pt-3">
          <div className="flex flex-col gap-2">
            {rows.map((row, i) => {
              const name = row.name.trim();
              const overridden = name in builtin && row.value !== builtin[name];
              const isNew = name !== "" && !(name in builtin);
              return (
                <div key={i} className="flex items-center gap-2">
                  <input
                    value={row.name}
                    onChange={(e) => setRow(i, "name", e.target.value)}
                    placeholder="Header-Name"
                    spellCheck={false}
                    className="w-44 rounded-md border border-border bg-background px-2 py-1.5 text-sm focus:border-primary focus:outline-none"
                  />
                  <input
                    value={row.value}
                    onChange={(e) => setRow(i, "value", e.target.value)}
                    placeholder="value"
                    spellCheck={false}
                    className="flex-1 rounded-md border border-border bg-background px-2 py-1.5 text-sm focus:border-primary focus:outline-none"
                  />
                  {overridden && (
                    <Badge variant="warning" size="sm">override</Badge>
                  )}
                  {isNew && <Badge variant="info" size="sm">new</Badge>}
                  <button
                    type="button"
                    onClick={() => removeRow(i)}
                    className="text-text-muted hover:text-danger"
                    title="Remove row"
                  >
                    <span className="material-symbols-outlined text-sm">close</span>
                  </button>
                </div>
              );
            })}
          </div>
          <div className="mt-3 flex items-center gap-2">
            <Button size="sm" variant="secondary" icon="add" onClick={addRow}>
              Add header
            </Button>
            <Button size="sm" variant="ghost" onClick={resetToBuiltin}>
              Reset to built-in
            </Button>
            <div className="flex-1" />
            <Button size="sm" onClick={save} disabled={saving}>
              {saving ? "Saving…" : "Save"}
            </Button>
          </div>
          <p className="mt-2 text-xs text-text-muted">
            Overrides are sent upstream after the built-in headers and replace
            any header with the same name. Host, Authorization, Cookie and
            other transport headers cannot be overridden.
          </p>
        </div>
      )}
    </Card>
  );
}
