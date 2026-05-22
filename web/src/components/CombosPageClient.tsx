"use client";

import { useState, useEffect, useCallback } from "react";
import { Card, Button, Modal, Input, CardSkeleton, ModelSelectModal, Toggle } from "@/shared/components";
import { ConfirmModal } from "@/shared/components/Modal";
import { useNotificationStore } from "@/store/notificationStore";
import { useCopyToClipboard } from "@/shared/hooks/useCopyToClipboard";
import { isOpenAICompatibleProvider, isAnthropicCompatibleProvider } from "@/shared/constants/providers";

// Validate combo name: only a-z, A-Z, 0-9, -, _
const VALID_NAME_REGEX = /^[a-zA-Z0-9_.\-]+$/;

interface Combo {
  id: string;
  name: string;
  models: string[];
  disabledModels?: string[];
  kind?: string;
}

interface Provider {
  id: string;
  provider: string;
  isActive?: boolean;
}

// Per-row result of the `/api/combos/test-model` ping. We keep it in the
// edit modal's local state only; the backend doesn't persist it because
// "did this model just respond?" is meaningful for ~seconds, not across
// sessions.
type ModelTestStatus = "idle" | "testing" | "ok" | "failed";

interface ModelTestResult {
  status: ModelTestStatus;
  latencyMs?: number;
  error?: string;
}

// Snapshot of `GET /api/combos/{id}/health` — purely UI surface so the
// operator can see which members are currently auto-quarantined after a
// recent failure and how long until they get retried.
interface ComboHealthEntry {
  model: string;
  remainingSeconds: number;
}

export default function CombosPage() {
  const [combos, setCombos] = useState<Combo[]>([]);
  const [loading, setLoading] = useState<boolean>(true);
  const [showCreateModal, setShowCreateModal] = useState<boolean>(false);
  const [editingCombo, setEditingCombo] = useState<Combo | null>(null);
  const [activeProviders, setActiveProviders] = useState<Provider[]>([]);
  const [comboStrategies, setComboStrategies] = useState<Record<string, any>>({});
  const [deleteTarget, setDeleteTarget] = useState<Combo | null>(null);
  const [deleting, setDeleting] = useState<boolean>(false);
  const notify = useNotificationStore();
  const { copied, copy } = useCopyToClipboard();

  useEffect(() => {
    fetchData();
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const fetchData = async () => {
    try {
      const [combosRes, providersRes, settingsRes] = await Promise.all([
        fetch("/api/combos"),
        fetch("/api/providers"),
        fetch("/api/settings"),
      ]);
      const combosData = await combosRes.json();
      const providersData = await providersRes.json();
      const settingsData = settingsRes.ok ? await settingsRes.json() : {};
      
      // Only LLM combos here — webSearch/webFetch combos belong to media-providers/web
      if (combosRes.ok) setCombos((combosData.combos || []).filter(c => !c.kind));
      if (providersRes.ok) {
        setActiveProviders(providersData.connections || []);
      }
      setComboStrategies(settingsData.comboStrategies || {});
    } catch (error) {
      console.log("Error fetching data:", error);
    } finally {
      setLoading(false);
    }
  };

  const handleCreate = async (data: { name: string; models: string[] }) => {
    try {
      const res = await fetch("/api/combos", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(data),
      });
      if (res.ok) {
        await fetchData();
        setShowCreateModal(false);
        notify.success(`Combo "${data.name}" created`);
      } else {
        const err = await res.json();
        notify.error(err.error || "Failed to create combo");
      }
    } catch (error) {
      console.log("Error creating combo:", error);
      notify.error("Failed to create combo");
    }
  };

  const handleUpdate = async (
    id: string,
    data: { name: string; models: string[]; disabledModels?: string[] },
  ) => {
    try {
      const res = await fetch(`/api/combos/${id}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(data),
      });
      if (res.ok) {
        await fetchData();
        setEditingCombo(null);
        notify.success(`Combo "${data.name}" updated`);
      } else {
        const err = await res.json();
        notify.error(err.error || "Failed to update combo");
      }
    } catch (error) {
      console.log("Error updating combo:", error);
      notify.error("Failed to update combo");
    }
  };

  const handleDelete = (combo: Combo) => {
    setDeleteTarget(combo);
  };

  const confirmDelete = async () => {
    if (!deleteTarget) return;
    setDeleting(true);
    try {
      const res = await fetch(`/api/combos/${deleteTarget.id}`, { method: "DELETE" });
      if (res.ok) {
        setCombos(combos.filter(c => c.id !== deleteTarget.id));
        notify.success(`Combo "${deleteTarget.name}" deleted`);
      } else {
        notify.error("Failed to delete combo");
      }
    } catch (error) {
      notify.error("Failed to delete combo");
      console.log("Error deleting combo:", error);
    }
  };

  const handleToggleRoundRobin = async (comboName: string, enabled: boolean) => {
    try {
      const updated = { ...comboStrategies };
      if (enabled) {
        updated[comboName] = { fallbackStrategy: "round-robin" };
      } else {
        delete updated[comboName];
      }

      await fetch("/api/settings", {
        method: "PATCH",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ comboStrategies: updated }),
      });

      setComboStrategies(updated);
    } catch (error) {
      console.log("Error updating combo strategy:", error);
    }
  };

  if (loading) {
    return (
      <div className="flex flex-col gap-6">
        <CardSkeleton />
        <CardSkeleton />
      </div>
    );
  }

  return (
    <div className="flex min-w-0 flex-col gap-6 px-1 sm:px-0">
      {/* Header */}
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div className="min-w-0">
          <h1 className="text-2xl font-semibold">Combos</h1>
          <p className="text-sm text-text-muted mt-1">
            Create model combos with fallback support
          </p>
        </div>
        <Button icon="add" onClick={() => setShowCreateModal(true)} className="w-full sm:w-auto">
          Create Combo
        </Button>
      </div>

      {/* Combos List */}
      {combos.length === 0 ? (
        <Card>
          <div className="text-center py-12">
            <div className="inline-flex items-center justify-center w-16 h-16 rounded-full bg-primary/10 text-primary mb-4">
              <span className="material-symbols-outlined text-[32px]">layers</span>
            </div>
            <p className="text-text-main font-medium mb-1">No combos yet</p>
            <p className="text-sm text-text-muted mb-4">Create model combos with fallback support</p>
            <Button icon="add" onClick={() => setShowCreateModal(true)} className="w-full sm:w-auto">
              Create Combo
            </Button>
          </div>
        </Card>
      ) : (
        <div className="flex flex-col gap-4">
          {combos.map((combo) => (
            <ComboCard
              key={combo.id}
              combo={combo}
              copied={copied}
              onCopy={copy}
              onEdit={() => setEditingCombo(combo)}
              onDelete={() => handleDelete(combo)}
              roundRobinEnabled={comboStrategies[combo.name]?.fallbackStrategy === "round-robin"}
              onToggleRoundRobin={(enabled) => handleToggleRoundRobin(combo.name, enabled)}
            />
          ))}
        </div>
      )}

      {/* Create Modal - Use key to force remount and reset state */}
      <ComboFormModal
        key="create"
        isOpen={showCreateModal}
        onClose={() => setShowCreateModal(false)}
        onSave={handleCreate}
        activeProviders={activeProviders}
      />

      {/* Edit Modal - Use key to force remount and reset state */}
      <ComboFormModal
        key={editingCombo?.id || "new"}
        isOpen={!!editingCombo}
        combo={editingCombo}
        onClose={() => setEditingCombo(null)}
        onSave={(data) => handleUpdate(editingCombo.id, data)}
        activeProviders={activeProviders}
      />

      <ConfirmModal
        isOpen={!!deleteTarget}
        onClose={() => setDeleteTarget(null)}
        onConfirm={async () => {
          await confirmDelete();
          setDeleteTarget(null);
          setDeleting(false);
        }}
        title="Delete combo"
        message={deleteTarget ? <>Are you sure you want to delete combo <code>{deleteTarget.name}</code>? This cannot be undone.</> : null}
        confirmText="Delete"
        variant="danger"
        loading={deleting}
      />
    </div>
  );
}

interface ComboCardProps {
  combo: Combo;
  copied: string | null;
  onCopy: (name: string, id: string) => void;
  onEdit: () => void;
  onDelete: () => void;
  roundRobinEnabled: boolean;
  onToggleRoundRobin: (enabled: boolean) => void;
}

function ComboCard({ combo, copied, onCopy, onEdit, onDelete, roundRobinEnabled, onToggleRoundRobin }: ComboCardProps) {
  const [health, setHealth] = useState<ComboHealthEntry[]>([]);

  // Poll the combo's quarantine state so the "cooling down" pill on the
  // card reflects the backend without having to open the edit modal.
  // Cheap call (in-memory map lookup) so 15s is plenty.
  useEffect(() => {
    let cancelled = false;
    const fetchHealth = async () => {
      try {
        const res = await fetch(`/api/combos/${combo.id}/health`);
        if (!res.ok) return;
        const data = await res.json();
        if (!cancelled) setHealth(data.quarantined || []);
      } catch {
        // silent — re-tried by interval
      }
    };
    fetchHealth();
    const interval = setInterval(fetchHealth, 15000);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [combo.id]);

  const disabled = combo.disabledModels || [];
  const quarantined = health;

  return (
    <Card padding="sm" className="group">
      <div className="flex min-w-0 flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex min-w-0 flex-1 items-start gap-3 sm:items-center">
          <div className="size-8 rounded-lg bg-primary/10 flex items-center justify-center shrink-0">
            <span className="material-symbols-outlined text-primary text-[18px]">layers</span>
          </div>
          <div className="min-w-0 flex-1">
            <code className="block truncate font-mono text-sm font-medium">{combo.name}</code>
            <div className="mt-1 flex min-w-0 flex-wrap items-center gap-1">
              {combo.models.length === 0 ? (
                <span className="text-xs text-text-muted italic">No models</span>
              ) : (
                combo.models.slice(0, 3).map((model, index) => {
                  const isDisabled = disabled.includes(model);
                  const isQuarantined = quarantined.some((q) => q.model === model);
                  return (
                    <code
                      key={index}
                      className={`max-w-full truncate rounded px-1.5 py-0.5 font-mono text-[10px] sm:max-w-[220px] ${
                        isDisabled
                          ? "bg-red-500/10 text-red-500 line-through"
                          : isQuarantined
                            ? "bg-amber-500/10 text-amber-600 dark:text-amber-400"
                            : "bg-black/5 text-text-muted dark:bg-white/5"
                      }`}
                      title={
                        isDisabled
                          ? "Disabled — never dispatched"
                          : isQuarantined
                            ? "Cooling down after recent failure"
                            : undefined
                      }
                    >
                      {model}
                    </code>
                  );
                })
              )}
              {combo.models.length > 3 && (
                <span className="text-[10px] text-text-muted">+{combo.models.length - 3} more</span>
              )}
              {(disabled.length > 0 || quarantined.length > 0) && (
                <div className="ml-1 flex items-center gap-1">
                  {disabled.length > 0 && (
                    <span
                      className="inline-flex items-center gap-0.5 rounded bg-red-500/10 px-1 py-0.5 text-[10px] font-medium text-red-500"
                      title={`${disabled.length} model(s) muted by you`}
                    >
                      <span className="material-symbols-outlined text-[10px]">block</span>
                      {disabled.length}
                    </span>
                  )}
                  {quarantined.length > 0 && (
                    <span
                      className="inline-flex items-center gap-0.5 rounded bg-amber-500/10 px-1 py-0.5 text-[10px] font-medium text-amber-600 dark:text-amber-400"
                      title={`${quarantined.length} model(s) cooling down after recent failure`}
                    >
                      <span className="material-symbols-outlined text-[10px]">schedule</span>
                      {quarantined.length}
                    </span>
                  )}
                </div>
              )}
            </div>
          </div>
        </div>

        {/* Actions */}
        <div className="flex w-full flex-col gap-2 sm:w-auto sm:flex-row sm:items-center sm:gap-3 sm:shrink-0">
          {/* Round Robin Toggle — always visible */}
          <div className="flex items-center justify-between gap-1.5 rounded-lg bg-black/[0.02] px-2 py-1.5 dark:bg-white/[0.02] sm:justify-start sm:bg-transparent sm:px-0 sm:py-0 sm:dark:bg-transparent">
            <span className="text-xs text-text-muted font-medium">Round Robin</span>
            <Toggle
              size="sm"
              checked={roundRobinEnabled}
              onChange={onToggleRoundRobin}
            />
          </div>

          <div className="grid grid-cols-3 gap-1 sm:flex">
            <button
              onClick={(e) => { e.stopPropagation(); onCopy(combo.name, `combo-${combo.id}`); }}
              className="flex flex-col items-center rounded px-2 py-1 text-text-muted transition-colors hover:bg-black/5 hover:text-primary dark:hover:bg-white/5"
              title="Copy combo name"
            >
              <span className="material-symbols-outlined text-[18px]">
                {copied === `combo-${combo.id}` ? "check" : "content_copy"}
              </span>
              <span className="text-[10px] leading-tight">Copy</span>
            </button>
            <button
              onClick={onEdit}
              className="flex flex-col items-center rounded px-2 py-1 text-text-muted transition-colors hover:bg-black/5 hover:text-primary dark:hover:bg-white/5"
              title="Edit"
            >
              <span className="material-symbols-outlined text-[18px]">edit</span>
              <span className="text-[10px] leading-tight">Edit</span>
            </button>
            <button
              onClick={onDelete}
              className="flex flex-col items-center rounded px-2 py-1 text-red-500 transition-colors hover:bg-red-500/10"
              title="Delete"
            >
              <span className="material-symbols-outlined text-[18px]">delete</span>
              <span className="text-[10px] leading-tight">Delete</span>
            </button>
          </div>
        </div>
      </div>
    </Card>
  );
}

// Inline editable model item
interface ModelItemProps {
  index: number;
  model: string;
  isDragging: boolean;
  isDragOver: boolean;
  // Per-combo-member health state. `disabled` is the persisted manual
  // mute that the dispatcher enforces; `testResult` is transient state
  // from clicking the test icon; `quarantineSeconds` is how long the
  // server says the model is auto-quarantined after a recent failure.
  disabled: boolean;
  testResult: ModelTestResult;
  quarantineSeconds?: number;
  onEdit: (newVal: string) => void;
  onToggleDisabled: () => void;
  onTest: () => void;
  onDragStart: (index: number) => void;
  onDragEnter: (index: number) => void;
  onDragEnd: () => void;
  onDrop: (index: number, from: number | null) => void;
  onRemove: () => void;
}

function ModelItem({
  index,
  model,
  isDragging,
  isDragOver,
  disabled,
  testResult,
  quarantineSeconds,
  onEdit,
  onToggleDisabled,
  onTest,
  onDragStart,
  onDragEnter,
  onDragEnd,
  onDrop,
  onRemove,
}: ModelItemProps) {
  const [editing, setEditing] = useState<boolean>(false);
  const [draft, setDraft] = useState<string>(model);

  const commit = () => {
    const trimmed = draft.trim();
    if (trimmed && trimmed !== model) onEdit(trimmed);
    else setDraft(model); // revert if empty or unchanged
    setEditing(false);
  };

  const handleKeyDown = (e) => {
    if (e.key === "Enter") commit();
    if (e.key === "Escape") { setDraft(model); setEditing(false); }
  };

  return (
    <div
      draggable={!editing}
      onDragStart={(e) => {
        e.dataTransfer.effectAllowed = "move";
        e.dataTransfer.setData("text/plain", String(index));
        onDragStart(index);
      }}
      onDragEnter={(e) => {
        e.preventDefault();
        onDragEnter(index);
      }}
      onDragOver={(e) => {
        e.preventDefault();
        e.dataTransfer.dropEffect = "move";
      }}
      onDrop={(e) => {
        e.preventDefault();
        const fromStr = e.dataTransfer.getData("text/plain");
        const from = fromStr === "" ? NaN : Number(fromStr);
        onDrop(index, Number.isFinite(from) ? from : null);
      }}
      onDragEnd={onDragEnd}
      className={`group flex min-w-0 items-center gap-1.5 rounded-md px-2 py-1 transition-all ${
        disabled
          ? "bg-red-500/[0.05] hover:bg-red-500/[0.08] dark:bg-red-500/[0.08] dark:hover:bg-red-500/[0.12]"
          : "bg-black/[0.02] hover:bg-black/[0.04] dark:bg-white/[0.02] dark:hover:bg-white/[0.04]"
      } ${isDragging ? "opacity-40" : ""} ${
        isDragOver && !isDragging
          ? "ring-2 ring-primary/60 ring-offset-1 ring-offset-bg dark:ring-offset-canvas"
          : ""
      }`}
    >
      {/* Drag handle */}
      <span
        className="material-symbols-outlined text-text-muted/70 cursor-grab active:cursor-grabbing text-[14px] shrink-0 hover:text-primary"
        title="Drag to reorder"
      >
        drag_indicator
      </span>

      {/* Index badge */}
      <span className="text-[10px] font-medium text-text-muted w-3 text-center shrink-0">{index + 1}</span>

      {/* Inline editable model value */}
      {editing ? (
        <input
          autoFocus
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={commit}
          onKeyDown={handleKeyDown}
          className="min-w-0 flex-1 rounded border border-primary/40 bg-white px-1.5 py-0.5 font-mono text-xs text-text-main outline-none dark:bg-black/20"
        />
      ) : (
        <div
          className={`min-w-0 flex-1 cursor-text truncate rounded px-1.5 py-0.5 font-mono text-xs hover:bg-black/5 dark:hover:bg-white/5 ${
            disabled ? "text-text-muted line-through" : "text-text-main"
          }`}
          onClick={() => setEditing(true)}
          title={disabled ? `${model} — disabled, never dispatched` : "Click to edit"}
        >
          {model}
        </div>
      )}

      {/* Test status badge (only shown after a test run) */}
      {testResult.status === "ok" && (
        <span
          className="inline-flex items-center gap-0.5 rounded bg-emerald-500/10 px-1 py-0.5 text-[10px] font-medium text-emerald-600 dark:text-emerald-400 shrink-0"
          title={`Last test ok in ${testResult.latencyMs}ms`}
        >
          <span className="material-symbols-outlined text-[10px]">check_circle</span>
          {testResult.latencyMs}ms
        </span>
      )}
      {testResult.status === "failed" && (
        <span
          className="inline-flex items-center gap-0.5 rounded bg-red-500/10 px-1 py-0.5 text-[10px] font-medium text-red-500 shrink-0 max-w-[160px] truncate"
          title={testResult.error || "Test failed"}
        >
          <span className="material-symbols-outlined text-[10px]">error</span>
          {testResult.error ? testResult.error.slice(0, 24) : "failed"}
        </span>
      )}

      {/* Auto-quarantine indicator (server-driven) */}
      {!disabled && quarantineSeconds !== undefined && quarantineSeconds > 0 && (
        <span
          className="inline-flex items-center gap-0.5 rounded bg-amber-500/10 px-1 py-0.5 text-[10px] font-medium text-amber-600 dark:text-amber-400 shrink-0"
          title={`Auto-quarantined after recent failure. Retries unlock in ${quarantineSeconds}s.`}
        >
          <span className="material-symbols-outlined text-[10px]">schedule</span>
          {quarantineSeconds}s
        </span>
      )}

      {/* Test */}
      <button
        onClick={(e) => {
          e.stopPropagation();
          onTest();
        }}
        disabled={testResult.status === "testing"}
        className={`p-0.5 rounded transition-all ${
          testResult.status === "testing"
            ? "text-primary animate-pulse"
            : "text-text-muted hover:text-primary hover:bg-primary/10"
        }`}
        title={testResult.status === "testing" ? "Testing…" : "Test this model"}
      >
        <span className="material-symbols-outlined text-[12px]">
          {testResult.status === "testing" ? "progress_activity" : "speed"}
        </span>
      </button>

      {/* Mute / unmute */}
      <button
        onClick={(e) => {
          e.stopPropagation();
          onToggleDisabled();
        }}
        className={`p-0.5 rounded transition-all ${
          disabled
            ? "text-red-500 hover:bg-red-500/10"
            : "text-text-muted hover:text-amber-600 hover:bg-amber-500/10"
        }`}
        title={
          disabled
            ? "Re-enable this combo member"
            : "Disable — keep in list but never dispatch to it"
        }
      >
        <span className="material-symbols-outlined text-[12px]">
          {disabled ? "block" : "visibility"}
        </span>
      </button>

      {/* Remove */}
      <button
        onClick={onRemove}
        className="p-0.5 hover:bg-red-500/10 rounded text-text-muted hover:text-red-500 transition-all"
        title="Remove"
      >
        <span className="material-symbols-outlined text-[12px]">close</span>
      </button>
    </div>
  );
}

interface ComboFormModalProps {
  isOpen: boolean;
  combo?: Combo | null;
  onClose: () => void;
  onSave: (data: { name: string; models: string[]; disabledModels?: string[] }) => void;
  activeProviders: Provider[];
  kindFilter?: string | null;
}

function ComboFormModal({ isOpen, combo, onClose, onSave, activeProviders, kindFilter = null }: ComboFormModalProps) {
  // Initialize state with combo values - key prop on parent handles reset on remount
  const [name, setName] = useState<string>(combo?.name || "");
  const [models, setModels] = useState<string[]>(combo?.models || []);
  const [disabledModels, setDisabledModels] = useState<string[]>(combo?.disabledModels || []);
  const [showModelSelect, setShowModelSelect] = useState<boolean>(false);
  const [saving, setSaving] = useState<boolean>(false);
  const [nameError, setNameError] = useState<string>("");
  const [modelAliases, setModelAliases] = useState<Record<string, any>>({});
  const [dragIndex, setDragIndex] = useState<number | null>(null);
  const [dragOverIndex, setDragOverIndex] = useState<number | null>(null);
  // Map of `<model>` → last test result; local to this modal lifecycle.
  const [testResults, setTestResults] = useState<Record<string, ModelTestResult>>({});
  // Map of `<model>` → remaining auto-quarantine seconds (server-driven).
  const [quarantine, setQuarantine] = useState<Record<string, number>>({});

  const fetchModalData = async () => {
    try {
      const aliasesRes = await fetch("/api/models/alias");
      if (!aliasesRes.ok) return;
      const aliasesData = await aliasesRes.json();
      setModelAliases(aliasesData.aliases || {});
    } catch (error) {
      console.error("Error fetching modal data:", error);
    }
  };

  // Refresh combo health (auto-quarantine map) so the modal mirrors what
  // the dispatcher would do on the next request. We only do this in edit
  // mode — the create flow doesn't have an id to look up yet.
  const fetchHealth = useCallback(async () => {
    if (!combo?.id) return;
    try {
      const res = await fetch(`/api/combos/${combo.id}/health`);
      if (!res.ok) return;
      const data = await res.json();
      const next: Record<string, number> = {};
      for (const entry of data.quarantined || []) {
        next[entry.model] = entry.remainingSeconds;
      }
      setQuarantine(next);
    } catch {
      // silent
    }
  }, [combo?.id]);

  useEffect(() => {
    if (isOpen) {
      fetchModalData();
      fetchHealth();
    }
  }, [isOpen, fetchHealth]);

  const runTest = async (model: string) => {
    setTestResults((prev) => ({ ...prev, [model]: { status: "testing" } }));
    try {
      const res = await fetch("/api/combos/test-model", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ model }),
      });
      const data = await res.json();
      setTestResults((prev) => ({
        ...prev,
        [model]: {
          status: data.ok ? "ok" : "failed",
          latencyMs: data.latencyMs,
          error: data.error,
        },
      }));
    } catch (error) {
      setTestResults((prev) => ({
        ...prev,
        [model]: {
          status: "failed",
          error: error instanceof Error ? error.message : "Test failed",
        },
      }));
    }
  };

  const runTestAll = async () => {
    await Promise.all(models.map((model) => runTest(model)));
  };

  const clearQuarantine = async () => {
    if (!combo?.id) return;
    try {
      await fetch(`/api/combos/${combo.id}/health`, { method: "DELETE" });
      await fetchHealth();
    } catch (error) {
      console.error("Error clearing quarantine:", error);
    }
  };

  const toggleDisabled = (model: string) => {
    setDisabledModels((prev) =>
      prev.includes(model) ? prev.filter((m) => m !== model) : [...prev, model],
    );
  };

  const validateName = (value: string): boolean => {
    if (!value.trim()) {
      setNameError("Name is required");
      return false;
    }
    if (!VALID_NAME_REGEX.test(value)) {
      setNameError("Only letters, numbers, -, _ and . allowed");
      return false;
    }
    setNameError("");
    return true;
  };

  const handleNameChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const value = e.target.value;
    setName(value);
    if (value) validateName(value);
    else setNameError("");
  };

  // Toggle in/out of combo so the modal can stay open while operators
  // pick several models, including unpicking ones added by mistake.
  const handleAddModel = (model: { value: string }) => {
    setModels((prev) =>
      prev.includes(model.value)
        ? prev.filter((m) => m !== model.value)
        : [...prev, model.value],
    );
  };

  const handleRemoveModel = (index: number) => {
    setModels(models.filter((_, i) => i !== index));
  };

  const handleReorder = (from: number, to: number) => {
    if (from === to || from < 0 || to < 0 || from >= models.length || to >= models.length) return;
    const next = [...models];
    const [moved] = next.splice(from, 1);
    next.splice(to, 0, moved);
    setModels(next);
  };

  const handleSave = async () => {
    if (!validateName(name)) return;
    setSaving(true);
    // Filter `disabledModels` down to only members that are still in the
    // configured list — anything removed via the trash icon shouldn't
    // linger in the disabled set on disk.
    const cleanedDisabled = disabledModels.filter((m) => models.includes(m));
    await onSave({ name: name.trim(), models, disabledModels: cleanedDisabled });
    setSaving(false);
  };

  const isEdit = !!combo;
  const hasQuarantine = Object.keys(quarantine).length > 0;

  return (
    <>
      <Modal
        isOpen={isOpen}
        onClose={onClose}
        title={isEdit ? "Edit Combo" : "Create Combo"}
        size="lg"
      >
        <div className="flex flex-col gap-3">
          {/* Name */}
          <div>
            <Input
              label="Combo Name"
              value={name}
              onChange={handleNameChange}
              placeholder="my-combo"
              error={nameError}
            />
            <p className="text-[10px] text-text-muted mt-0.5">
              Only letters, numbers, -, _ and . allowed
            </p>
          </div>

          {/* Models */}
          <div>
            <div className="flex items-center justify-between mb-1.5">
              <label className="text-sm font-medium block">Models</label>
              {models.length > 0 && (
                <div className="flex items-center gap-1">
                  {hasQuarantine && isEdit && (
                    <button
                      type="button"
                      onClick={clearQuarantine}
                      className="inline-flex items-center gap-1 rounded-md border border-amber-500/30 bg-amber-500/5 px-2 py-1 text-[11px] font-medium text-amber-700 hover:bg-amber-500/10 dark:text-amber-400"
                      title="Clear the auto-quarantine cooldowns so the dispatcher retries those members on the next request."
                    >
                      <span className="material-symbols-outlined text-[12px]">refresh</span>
                      Clear cooldowns
                    </button>
                  )}
                  <button
                    type="button"
                    onClick={runTestAll}
                    className="inline-flex items-center gap-1 rounded-md border border-primary/30 bg-primary/5 px-2 py-1 text-[11px] font-medium text-primary hover:bg-primary/10"
                    title="Ping every member with max_tokens=1 to spot broken ones quickly."
                  >
                    <span className="material-symbols-outlined text-[12px]">speed</span>
                    Test all
                  </button>
                </div>
              )}
            </div>

            {models.length === 0 ? (
              <div className="text-center py-4 border border-dashed border-black/10 dark:border-white/10 rounded-lg bg-black/[0.01] dark:bg-white/[0.01]">
                <span className="material-symbols-outlined text-text-muted text-xl mb-1">layers</span>
                <p className="text-xs text-text-muted">No models added yet</p>
              </div>
            ) : (
            <div className="flex max-h-[55vh] min-w-0 flex-col gap-1 overflow-y-auto sm:max-h-[350px]">
                {models.map((model, index) => (
                  <ModelItem
                    key={index}
                    index={index}
                    model={model}
                    isDragging={dragIndex === index}
                    isDragOver={dragOverIndex === index}
                    disabled={disabledModels.includes(model)}
                    testResult={testResults[model] || { status: "idle" }}
                    quarantineSeconds={quarantine[model]}
                    onEdit={(newVal) => {
                      const updated = [...models];
                      updated[index] = newVal;
                      // Migrate disabled flag if the user renamed in
                      // place so we don't strand the old entry.
                      setDisabledModels((prev) =>
                        prev.map((m) => (m === model ? newVal : m)),
                      );
                      setModels(updated);
                    }}
                    onToggleDisabled={() => toggleDisabled(model)}
                    onTest={() => runTest(model)}
                    onDragStart={setDragIndex}
                    onDragEnter={setDragOverIndex}
                    onDragEnd={() => {
                      setDragIndex(null);
                      setDragOverIndex(null);
                    }}
                    onDrop={(target, from) => {
                      const src = from ?? dragIndex;
                      if (src !== null && src !== undefined) handleReorder(src, target);
                      setDragIndex(null);
                      setDragOverIndex(null);
                    }}
                    onRemove={() => handleRemoveModel(index)}
                  />
                ))}
              </div>
            )}

            {/* Add Model button */}
            <button
              onClick={() => setShowModelSelect(true)}
              className="w-full mt-2 py-2 border border-dashed border-black/10 dark:border-white/10 rounded-lg text-xs text-primary font-medium hover:text-primary hover:border-primary/50 transition-colors flex items-center justify-center gap-1"
            >
              <span className="material-symbols-outlined text-[16px]">add</span>
              Add Model
            </button>
          </div>

          {/* Actions */}
          <div className="flex flex-col gap-2 pt-1 sm:flex-row">
            <Button onClick={onClose} variant="ghost" fullWidth size="sm">
              Cancel
            </Button>
            <Button
              onClick={handleSave}
              fullWidth
              size="sm"
              disabled={!name.trim() || !!nameError || saving}
            >
              {saving ? "Saving..." : isEdit ? "Save" : "Create"}
            </Button>
          </div>
        </div>
      </Modal>

      {/* Model Select Modal */}
      <ModelSelectModal
        isOpen={showModelSelect}
        onClose={() => setShowModelSelect(false)}
        onSelect={handleAddModel}
        selectedModel={models}
        closeOnSelect={false}
        activeProviders={activeProviders}
        modelAliases={modelAliases}
        title="Add Models to Combo"
        kindFilter={kindFilter}
      />
    </>
  );
}
