"use client";

import { useState } from "react";
import { Button, Modal } from "@/shared/components";

const PLACEHOLDER = `[
  {
    "accessToken": "eyJhbGc...",
    "refreshToken": "rt_...",
    "idToken": "eyJhbGc...",
    "email": "user@example.com"
  }
]`;

function normalizeToArray(parsed: unknown): Record<string, unknown>[] | null {
  if (Array.isArray(parsed)) return parsed as Record<string, unknown>[];
  if (parsed && typeof parsed === "object") {
    const obj = parsed as Record<string, unknown>;
    if (Array.isArray(obj.accounts)) return obj.accounts as Record<string, unknown>[];
    return [obj];
  }
  return null;
}

interface BulkImportCodexModalProps {
  isOpen: boolean;
  onClose: () => void;
  onSuccess?: () => void;
}

interface ImportResult {
  success: number;
  failed: number;
  results?: { index: number; ok: boolean; error?: string }[];
}

export default function BulkImportCodexModal({ isOpen, onClose, onSuccess }: BulkImportCodexModalProps): React.ReactNode {
  const [jsonText, setJsonText] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [parseError, setParseError] = useState("");
  const [result, setResult] = useState<ImportResult | null>(null);

  const handleClose = () => {
    if (submitting) return;
    setJsonText("");
    setParseError("");
    setResult(null);
    onClose();
  };

  const handleSubmit = async () => {
    setParseError("");
    setResult(null);

    const trimmed = jsonText.trim();
    if (!trimmed) return;

    let parsed: unknown;
    try {
      parsed = JSON.parse(trimmed);
    } catch (err) {
      setParseError(`Invalid JSON: ${(err as Error).message}`);
      return;
    }

    const accounts = normalizeToArray(parsed);
    if (!accounts || accounts.length === 0) {
      setParseError("No accounts found in input");
      return;
    }

    setSubmitting(true);
    try {
      const res = await fetch("/api/oauth/codex/bulk-import", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ accounts }),
      });
      const data: ImportResult = await res.json();
      if (!res.ok) {
        setParseError((data as any)?.error || `Request failed: ${res.status}`);
        return;
      }
      setResult(data);
      if (data.success > 0 && typeof onSuccess === "function") {
        onSuccess();
      }
    } catch (err) {
      setParseError((err as Error).message || "Request failed");
    } finally {
      setSubmitting(false);
    }
  };

  const failedItems = result?.results?.filter((r) => !r.ok) || [];

  return (
    <Modal isOpen={isOpen} title="Bulk Add Codex Accounts" onClose={handleClose}>
      <div className="flex flex-col gap-4">
        <p className="text-xs text-text-muted">
          Paste an array of codex account JSON objects. Each must include accessToken (and ideally refreshToken, idToken).
        </p>

        <textarea
          className="w-full rounded border border-accent/30 bg-sidebar p-2 text-sm font-mono resize-y min-h-[240px] focus:outline-none focus:ring-1 focus:ring-primary"
          placeholder={PLACEHOLDER}
          value={jsonText}
          onChange={(e) => setJsonText(e.target.value)}
          disabled={submitting}
        />

        {parseError && (
          <p className="text-xs text-red-500 break-words">{parseError}</p>
        )}

        {result && (
          <div className="flex flex-col gap-2">
            <div
              className={`text-sm font-medium ${
                result.failed > 0 ? "text-yellow-400" : "text-green-400"
              }`}
            >
              {result.success > 0 && `✓ ${result.success} added`}
              {result.failed > 0 ? `, ✗ ${result.failed} failed` : ""}
            </div>
            {failedItems.length > 0 && (
              <ul className="rounded border border-accent/20 bg-sidebar/50 p-2 text-xs font-mono max-h-40 overflow-y-auto">
                {failedItems.map((item) => (
                  <li key={item.index} className="text-red-400">
                    [{item.index}] {item.error}
                  </li>
                ))}
              </ul>
            )}
          </div>
        )}

        <div className="flex gap-2">
          <Button
            onClick={handleSubmit}
            fullWidth
            disabled={submitting || !jsonText.trim()}
          >
            {submitting ? "Importing..." : "Import All"}
          </Button>
          <Button onClick={handleClose} variant="ghost" fullWidth disabled={submitting}>
            Close
          </Button>
        </div>
      </div>
    </Modal>
  );
}
