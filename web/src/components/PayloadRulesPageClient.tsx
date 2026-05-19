"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import { Card, Button, CardSkeleton } from "@/shared/components";

// ──────────────────────────────────────────────────────────────────────
// Types — mirror the Rust backend (src/payload_rules.rs).
// ──────────────────────────────────────────────────────────────────────

interface ModelSpec {
  name: string;
  protocol?: string;
}

interface MutationRule {
  models: ModelSpec[];
  params: Record<string, unknown>;
}

interface FilterRule {
  models: ModelSpec[];
  params: string[];
}

interface PayloadRulesConfig {
  default: MutationRule[];
  override: MutationRule[];
  filter: FilterRule[];
  defaultRaw: MutationRule[];
}

interface PayloadRulesSummary {
  default: number;
  override: number;
  filter: number;
  defaultRaw: number;
}

type SystemPromptMode = "off" | "prepend" | "override";

interface SystemPromptConfig {
  mode: SystemPromptMode;
  content: string;
  active?: boolean;
}

type StatusMessage = { type: "success" | "error" | "info"; text: string };

const EMPTY_CONFIG: PayloadRulesConfig = {
  default: [],
  override: [],
  filter: [],
  defaultRaw: [],
};
const EMPTY_CONFIG_TEXT = JSON.stringify(EMPTY_CONFIG, null, 2);

// Example shown above the editor — copy-paste friendly, mirrors the
// shape described in PR-A1's commit message.
const EXAMPLE_CONFIG = `{
  "default": [
    { "models": [{ "name": "gpt-4*" }],
      "params": { "temperature": 0.2 } }
  ],
  "override": [
    { "models": [{ "name": "o1*", "protocol": "openai" }],
      "params": { "reasoning_effort": "medium", "max_tokens": 4096 } }
  ],
  "filter": [
    { "models": [{ "name": "claude-*" }],
      "params": ["metadata.user_id"] }
  ],
  "defaultRaw": [
    { "models": [{ "name": "*" }],
      "params": { "stream_options": { "include_usage": true } } }
  ]
}`;

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

function summarize(config: PayloadRulesConfig | null): PayloadRulesSummary {
  if (!config) return { default: 0, override: 0, filter: 0, defaultRaw: 0 };
  return {
    default: config.default?.length ?? 0,
    override: config.override?.length ?? 0,
    filter: config.filter?.length ?? 0,
    defaultRaw: config.defaultRaw?.length ?? 0,
  };
}

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

// ──────────────────────────────────────────────────────────────────────
// Payload-rules editor
// ──────────────────────────────────────────────────────────────────────

function PayloadRulesEditor() {
  const [editorValue, setEditorValue] = useState<string>(EMPTY_CONFIG_TEXT);
  const [loading, setLoading] = useState<boolean>(true);
  const [saving, setSaving] = useState<boolean>(false);
  const [serverSummary, setServerSummary] = useState<PayloadRulesSummary | null>(null);
  const [status, setStatus] = useState<StatusMessage | null>(null);

  const parsed = useMemo(() => {
    try {
      const value = JSON.parse(editorValue);
      if (!isRecord(value)) {
        return { config: null, error: "Top-level value must be a JSON object." };
      }
      return { config: value as PayloadRulesConfig, error: null };
    } catch (err) {
      const message = err instanceof Error ? err.message : "Invalid JSON";
      return { config: null, error: message };
    }
  }, [editorValue]);

  const localSummary = summarize(parsed.config);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const res = await fetch("/api/settings/payload-rules");
        if (!res.ok) {
          throw new Error(`Server returned ${res.status}`);
        }
        const data = await res.json();
        if (cancelled) return;
        const cfg = (data?.config as PayloadRulesConfig | undefined) ?? EMPTY_CONFIG;
        setEditorValue(JSON.stringify(cfg, null, 2));
        if (data?.summary) setServerSummary(data.summary as PayloadRulesSummary);
      } catch (err) {
        if (cancelled) return;
        setStatus({
          type: "error",
          text:
            err instanceof Error
              ? `Failed to load payload rules: ${err.message}`
              : "Failed to load payload rules",
        });
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const handleSave = useCallback(async () => {
    if (parsed.error || !parsed.config) {
      setStatus({ type: "error", text: parsed.error || "Invalid JSON" });
      return;
    }
    setSaving(true);
    setStatus(null);
    try {
      const res = await fetch("/api/settings/payload-rules", {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(parsed.config),
      });
      if (!res.ok) {
        const text = await res.text();
        throw new Error(text || `Server returned ${res.status}`);
      }
      const data = await res.json();
      const cfg = (data?.config as PayloadRulesConfig | undefined) ?? EMPTY_CONFIG;
      setEditorValue(JSON.stringify(cfg, null, 2));
      if (data?.summary) setServerSummary(data.summary as PayloadRulesSummary);
      setStatus({ type: "success", text: "Payload rules saved." });
    } catch (err) {
      setStatus({
        type: "error",
        text:
          err instanceof Error
            ? `Failed to save: ${err.message}`
            : "Failed to save payload rules",
      });
    } finally {
      setSaving(false);
    }
  }, [parsed]);

  const handleReset = useCallback(() => {
    setEditorValue(EMPTY_CONFIG_TEXT);
    setStatus({ type: "info", text: "Cleared editor — click Save to persist." });
  }, []);

  const handleExample = useCallback(() => {
    setEditorValue(EXAMPLE_CONFIG);
    setStatus({ type: "info", text: "Loaded example. Click Save to apply." });
  }, []);

  if (loading) {
    return <CardSkeleton />;
  }

  return (
    <Card className="p-5">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <h2 className="text-base font-bold text-text-main">Payload Rules</h2>
          <p className="mt-1 text-xs text-text-muted">
            Apply request transformations to chat completions based on model name. Four
            rule kinds (<code>default</code>, <code>override</code>, <code>filter</code>,
            <code>defaultRaw</code>) match against the request&apos;s <code>model</code> field
            via glob (<code>*</code>, <code>?</code>).
          </p>
        </div>
        <div className="flex gap-2">
          <Button size="sm" variant="ghost" onClick={handleExample}>
            Load example
          </Button>
          <Button size="sm" variant="ghost" onClick={handleReset}>
            Clear
          </Button>
        </div>
      </div>

      <SummaryRow
        label="On-disk (last saved)"
        summary={serverSummary ?? { default: 0, override: 0, filter: 0, defaultRaw: 0 }}
        tone="muted"
      />
      <SummaryRow label="Editor (unsaved)" summary={localSummary} tone="primary" />

      <textarea
        className="mt-3 h-72 w-full resize-y rounded-md border border-border bg-bg-subtle p-3 font-mono text-xs leading-relaxed text-text-main focus:border-primary focus:outline-none"
        value={editorValue}
        onChange={(e) => setEditorValue(e.target.value)}
        spellCheck={false}
        aria-label="Payload rules JSON editor"
      />

      {parsed.error ? (
        <div className="mt-2 rounded-md border border-red-300 bg-red-50 px-3 py-2 text-xs text-red-800 dark:border-red-700 dark:bg-red-900/30 dark:text-red-200">
          JSON error: {parsed.error}
        </div>
      ) : (
        <div className="mt-2 text-xs text-text-muted">JSON is valid.</div>
      )}

      <div className="mt-4 flex items-center justify-end gap-2">
        <Button
          variant="primary"
          onClick={handleSave}
          disabled={!!parsed.error || saving}
        >
          {saving ? "Saving…" : "Save"}
        </Button>
      </div>

      <StatusAlert status={status} />
    </Card>
  );
}

function SummaryRow({
  label,
  summary,
  tone,
}: {
  label: string;
  summary: PayloadRulesSummary;
  tone: "muted" | "primary";
}) {
  const pill = tone === "primary" ? "bg-primary/10 text-primary" : "bg-black/5 text-text-muted dark:bg-white/5";
  return (
    <div className="mt-3 flex flex-wrap items-center gap-2 text-xs">
      <span className="text-text-muted">{label}:</span>
      <span className={`rounded-full px-2 py-0.5 ${pill}`}>default {summary.default}</span>
      <span className={`rounded-full px-2 py-0.5 ${pill}`}>override {summary.override}</span>
      <span className={`rounded-full px-2 py-0.5 ${pill}`}>filter {summary.filter}</span>
      <span className={`rounded-full px-2 py-0.5 ${pill}`}>defaultRaw {summary.defaultRaw}</span>
    </div>
  );
}

// ──────────────────────────────────────────────────────────────────────
// System-prompt override
// ──────────────────────────────────────────────────────────────────────

function SystemPromptOverride() {
  const [config, setConfig] = useState<SystemPromptConfig>({ mode: "off", content: "" });
  const [loading, setLoading] = useState<boolean>(true);
  const [saving, setSaving] = useState<boolean>(false);
  const [status, setStatus] = useState<StatusMessage | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const res = await fetch("/api/settings/system-prompt");
        if (!res.ok) throw new Error(`Server returned ${res.status}`);
        const data = await res.json();
        if (cancelled) return;
        setConfig({
          mode: (data?.mode as SystemPromptMode) ?? "off",
          content: typeof data?.content === "string" ? data.content : "",
          active: !!data?.active,
        });
      } catch (err) {
        if (cancelled) return;
        setStatus({
          type: "error",
          text:
            err instanceof Error
              ? `Failed to load system prompt: ${err.message}`
              : "Failed to load system prompt",
        });
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const handleSave = useCallback(async () => {
    setSaving(true);
    setStatus(null);
    try {
      const res = await fetch("/api/settings/system-prompt", {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ mode: config.mode, content: config.content }),
      });
      if (!res.ok) {
        const text = await res.text();
        throw new Error(text || `Server returned ${res.status}`);
      }
      const data = await res.json();
      setConfig({
        mode: (data?.mode as SystemPromptMode) ?? "off",
        content: typeof data?.content === "string" ? data.content : "",
        active: !!data?.active,
      });
      setStatus({ type: "success", text: "System prompt saved." });
    } catch (err) {
      setStatus({
        type: "error",
        text:
          err instanceof Error
            ? `Failed to save: ${err.message}`
            : "Failed to save system prompt",
      });
    } finally {
      setSaving(false);
    }
  }, [config.mode, config.content]);

  if (loading) {
    return <CardSkeleton />;
  }

  const modeDescriptions: Record<SystemPromptMode, string> = {
    off: "Pass through caller's system messages unchanged.",
    prepend: "Insert this prompt as the first system message ONLY when the caller did not send one.",
    override: "Replace any caller-supplied system message with this prompt.",
  };

  return (
    <Card className="p-5">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <h2 className="text-base font-bold text-text-main">System Prompt Override</h2>
          <p className="mt-1 text-xs text-text-muted">
            Inject or replace the <code>system</code> message on every chat
            completions request. Applied before payload rules.
          </p>
        </div>
        <span
          className={`rounded-full px-2 py-0.5 text-[10px] font-semibold ${
            config.active
              ? "bg-green-100 text-green-800 dark:bg-green-900/40 dark:text-green-200"
              : "bg-black/5 text-text-muted dark:bg-white/5"
          }`}
        >
          {config.active ? "ACTIVE" : "INACTIVE"}
        </span>
      </div>

      <fieldset className="mt-4 space-y-2">
        {(Object.keys(modeDescriptions) as SystemPromptMode[]).map((mode) => (
          <label
            key={mode}
            className={`flex cursor-pointer items-start gap-3 rounded-md border p-3 text-sm transition-colors ${
              config.mode === mode
                ? "border-primary bg-primary/5"
                : "border-border bg-bg-subtle hover:border-primary/40"
            }`}
          >
            <input
              type="radio"
              name="system-prompt-mode"
              value={mode}
              checked={config.mode === mode}
              onChange={() => setConfig((prev) => ({ ...prev, mode }))}
              className="mt-0.5"
            />
            <div className="min-w-0">
              <div className="font-semibold capitalize text-text-main">{mode}</div>
              <div className="text-xs text-text-muted">{modeDescriptions[mode]}</div>
            </div>
          </label>
        ))}
      </fieldset>

      <textarea
        className="mt-4 h-40 w-full resize-y rounded-md border border-border bg-bg-subtle p-3 font-mono text-xs leading-relaxed text-text-main focus:border-primary focus:outline-none"
        value={config.content}
        onChange={(e) => setConfig((prev) => ({ ...prev, content: e.target.value }))}
        placeholder="Enter the system prompt content to inject / override."
        disabled={config.mode === "off"}
        aria-label="System prompt content"
      />

      <div className="mt-4 flex items-center justify-end gap-2">
        <Button variant="primary" onClick={handleSave} disabled={saving}>
          {saving ? "Saving…" : "Save"}
        </Button>
      </div>

      <StatusAlert status={status} />
    </Card>
  );
}

export default function PayloadRulesPageClient() {
  return (
    <div className="mx-auto flex max-w-5xl flex-col gap-6 p-4 sm:p-6">
      <header>
        <h1 className="text-2xl font-bold text-text-main">Payload Rules &amp; System Prompt</h1>
        <p className="mt-1 text-sm text-text-muted">
          Tweak request payloads and inject system prompts at the proxy layer —
          inspired by OmniRoute&apos;s <code>payloadRules</code> and{" "}
          <code>system-prompt</code> APIs.
        </p>
      </header>

      <SystemPromptOverride />
      <PayloadRulesEditor />
    </div>
  );
}
