"use client";

import { useState, useEffect, useCallback } from "react";
import { Card, Button, Modal, Input, CardSkeleton, ModelSelectModal, Select, CapacityBadges, ComboFormModal, Toggle } from "@/shared/components";
import { ConfirmModal } from "@/shared/components/Modal";
import { useNotificationStore } from "@/store/notificationStore";
import { useSettingsStore } from "@/store/settingsStore";
import { useCopyToClipboard } from "@/shared/hooks/useCopyToClipboard";
import { useModelCaps } from "@/shared/hooks/useModelCaps";
import { isOpenAICompatibleProvider, isAnthropicCompatibleProvider } from "@/shared/constants/providers";
import type { ComboStrategyConfig, ComboStrategyOption } from "@/types";

const STRATEGY_OPTIONS: ComboStrategyOption[] = [
  { value: "fallback", label: "Fallback — try in order" },
  { value: "round-robin", label: "Round Robin — rotate" },
  { value: "fusion", label: "Fusion — panel + judge" },
  { value: "cheapest", label: "Cheapest — free/lowest cost first" },
  { value: "fastest", label: "Fastest — lowest latency first" },
  { value: "quality", label: "Quality — capability tier first" },
];

export type { Combo, ComboFormProvider, ComboHealthEntry } from "@/shared/components/ComboFormModal";
import type { ComboFormProvider as Provider } from "@/shared/components/ComboFormModal";

// Capacity adapter entry (one capability pool). Entry form mirrors
// 9router EMPTY_CAP_ENTRY; legacy array form normalized on load.
export interface CapacityAdapterEntry {
  enabled: boolean;
  roundRobin: boolean;
  models: string[];
}

const CAPACITY_ADAPTER_CAPS = [
  { key: "vision", label: "Vision", icon: "visibility", desc: "Images" },
  // pdf, videoInput temporarily hidden — no translator support yet for those blocks.
  { key: "audioInput", label: "Audio", icon: "graphic_eq", desc: "Audio input" },
];
const DEFAULT_FALLBACK_MODEL = "oc/mimo-v2.5-free";
const EMPTY_CAP_ENTRY: CapacityAdapterEntry = { enabled: true, roundRobin: false, models: [] };
const EMPTY_CAPACITY_ADAPTER: Record<string, CapacityAdapterEntry> = {
  vision: { ...EMPTY_CAP_ENTRY },
  pdf: { ...EMPTY_CAP_ENTRY },
  audioInput: { ...EMPTY_CAP_ENTRY },
  videoInput: { ...EMPTY_CAP_ENTRY },
};
// Backward-compat: legacy stored form was an array of {model, enabled}.
function normalizeCapEntry(entry: any): CapacityAdapterEntry {
  if (Array.isArray(entry)) {
    return { enabled: true, roundRobin: false, models: entry.map((e: any) => e?.model || e).filter(Boolean) };
  }
  if (entry && typeof entry === "object") {
    return {
      enabled: entry.enabled !== false,
      roundRobin: !!entry.roundRobin,
      models: Array.isArray(entry.models) ? entry.models.filter(Boolean) : [],
    };
  }
  return { ...EMPTY_CAP_ENTRY };
}

/** Normalize settings.comboStrategies[name] which may be a bare string or nested object. */
function normalizeStrategy(entry: unknown): ComboStrategyConfig {
  if (!entry) return {};
  if (typeof entry === "string") return { fallbackStrategy: entry };
  if (typeof entry === "object") return entry as ComboStrategyConfig;
  return {};
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
  const { getCaps } = useModelCaps();

  // Capacity adapter: global fallback pools of models per input-modality capability.
  // A request needing a capability the target model/combo lacks switches straight
  // to the first enabled model here instead of erroring or dropping the data.
  // Ported from 9router combos/page.js (backend: Rust capacity_adapter.rs).
  const [capacityAdapter, setCapacityAdapter] = useState<Record<string, CapacityAdapterEntry>>(EMPTY_CAPACITY_ADAPTER);

  const handleSetCapacityAdapter = async (next: Record<string, CapacityAdapterEntry>) => {
    setCapacityAdapter(next);
    try {
      await useSettingsStore.getState().patchSettings({ capacityAdapter: next });
    } catch (error) {
      console.log("Error updating capacity adapter:", error);
    }
  };

  useEffect(() => {
    fetchData();
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const fetchData = async () => {
    try {
      const [combosRes, providersRes, settingsData] = await Promise.all([
        fetch("/api/combos"),
        fetch("/api/providers"),
        useSettingsStore.getState().fetchSettings(),
      ]);
      const combosData = await combosRes.json();
      const providersData = await providersRes.json();
      const settingsSafe = settingsData || {};

      // Only LLM combos here — webSearch/webFetch combos belong to media-providers/web
      if (combosRes.ok) setCombos((combosData.combos || []).filter(c => !c.kind || c.kind === "llm"));
      if (providersRes.ok) {
        setActiveProviders(providersData.connections || []);
      }
      setComboStrategies(settingsSafe.comboStrategies || {});
      const rawAdapter = settingsSafe.capacityAdapter || {};
      const normalized: Record<string, CapacityAdapterEntry> = {};
      for (const cap of CAPACITY_ADAPTER_CAPS) {
        normalized[cap.key] = normalizeCapEntry(rawAdapter[cap.key]);
      }
      setCapacityAdapter(normalized);
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

  // Merge a per-combo strategy patch into settings.comboStrategies.
  // Default fallback with no extras drops the entry (keeps settings clean).
  const handleSetComboStrategy = async (
    comboName: string,
    patch: Partial<ComboStrategyConfig>,
  ) => {
    try {
      const updated = { ...comboStrategies };
      const next: ComboStrategyConfig = {
        ...normalizeStrategy(updated[comboName]),
        ...patch,
      };
      if (!next.fallbackStrategy || next.fallbackStrategy === "fallback") {
        delete updated[comboName];
      } else {
        updated[comboName] = next;
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
            Group models under one name, then pick a strategy per combo:
          </p>
          <ul className="text-sm text-text-muted mt-2 flex flex-col gap-1">
            <li>
              <span className="font-medium text-text-main">Fallback</span> — tries models in order
              (next on failure)
            </li>
            <li>
              <span className="font-medium text-text-main">Round Robin</span> — rotates models across
              requests to spread load
            </li>
            <li>
              <span className="font-medium text-text-main">Fusion</span> — queries all models in
              parallel, then a judge synthesizes one answer. Best quality, but costs the most: every
              request bills all panel models + the judge (N+1 calls)
            </li>
          </ul>
        </div>
        <Button icon="add" onClick={() => setShowCreateModal(true)} className="w-full sm:w-auto whitespace-nowrap">
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
              activeProviders={activeProviders}
              copied={copied}
              onCopy={copy}
              onEdit={() => setEditingCombo(combo)}
              onDelete={() => handleDelete(combo)}
              strategy={normalizeStrategy(comboStrategies[combo.name])}
              onSetStrategy={(patch) => handleSetComboStrategy(combo.name, patch)}
              getCaps={getCaps}
            />
          ))}
        </div>
      )}

      {/* Capacity Adapter */}
      <CapacityAdapterSection
        capacityAdapter={capacityAdapter}
        onChange={handleSetCapacityAdapter}
        activeProviders={activeProviders}
        getCaps={getCaps}
      />

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
  activeProviders: Provider[];
  copied: string | null;
  onCopy: (name: string, id: string) => void;
  onEdit: () => void;
  onDelete: () => void;
  strategy: ComboStrategyConfig;
  onSetStrategy: (patch: Partial<ComboStrategyConfig>) => void;
  getCaps?: (model: string) => import("@/shared/constants/models").ModelCaps | null | undefined;
}

function ComboCard({
  combo,
  activeProviders,
  copied,
  onCopy,
  onEdit,
  onDelete,
  strategy,
  onSetStrategy,
  getCaps,
}: ComboCardProps) {
  const [health, setHealth] = useState<ComboHealthEntry[]>([]);
  const [showJudgeSelect, setShowJudgeSelect] = useState(false);

  const current = strategy.fallbackStrategy || "fallback";
  const judge = strategy.judgeModel || "";
  const isFusion = current === "fusion";

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
                      className={`inline-flex max-w-full items-center gap-1 truncate rounded px-1.5 py-0.5 font-mono text-[10px] sm:max-w-[220px] ${
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
                      <span className="truncate">{model}</span>
                      {getCaps && <CapacityBadges caps={getCaps(model)} size={11} colorOverride="text-text-muted/70" />}
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
            {/* Fusion: judge picker (Auto = first model) */}
            {isFusion && (
              <div className="mt-2 flex min-w-0 flex-wrap items-center gap-1.5">
                <span className="text-[11px] font-medium text-text-muted">Judge</span>
                <button
                  type="button"
                  onClick={() => setShowJudgeSelect(true)}
                  className="inline-flex max-w-full items-center gap-1 rounded border border-dashed border-primary/40 px-1.5 py-0.5 font-mono text-[11px] text-primary hover:border-primary hover:bg-primary/5 transition-colors"
                  title="Pick the model that fuses panel answers"
                >
                  <span className="material-symbols-outlined text-[13px]">gavel</span>
                  <span className="truncate">
                    {judge || `Auto — ${combo.models[0] || "first model"}`}
                  </span>
                </button>
                {judge && (
                  <button
                    type="button"
                    onClick={() => onSetStrategy({ judgeModel: "" })}
                    className="p-0.5 rounded text-text-muted hover:text-red-500 hover:bg-red-500/10 transition-colors"
                    title="Reset judge to Auto"
                  >
                    <span className="material-symbols-outlined text-[13px]">close</span>
                  </button>
                )}
              </div>
            )}
          </div>
        </div>

        {/* Actions */}
        <div className="flex w-full flex-col gap-2 sm:w-auto sm:flex-row sm:items-center sm:gap-3 sm:shrink-0">
          {/* Strategy selector — always visible */}
          <div className="w-full sm:w-[200px]">
            <Select
              options={STRATEGY_OPTIONS}
              value={current}
              onChange={(e) => onSetStrategy({ fallbackStrategy: e.target.value })}
              selectClassName="py-1.5 text-xs"
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

      {/* Judge model picker */}
      <ModelSelectModal
        isOpen={showJudgeSelect}
        onClose={() => setShowJudgeSelect(false)}
        onSelect={(m) => {
          onSetStrategy({ judgeModel: m?.value || "" });
          setShowJudgeSelect(false);
        }}
        selectedModel={judge || undefined}
        activeProviders={activeProviders}
        title="Select Judge Model"
        closeOnSelect={true}
      />
    </Card>
  );
}

// ComboFormModal lives in shared (reused by CombosPageClient + media combo pages).
// Re-exported for backward compat; prefer importing from "@/shared/components".
export { ComboFormModal } from "@/shared/components/ComboFormModal";
export type { ComboFormModalProps, ModelTestResult } from "@/shared/components/ComboFormModal";

function CapacityAdapterSection({
  capacityAdapter,
  onChange,
  activeProviders,
  getCaps,
}: {
  capacityAdapter: Record<string, CapacityAdapterEntry>;
  onChange: (next: Record<string, CapacityAdapterEntry>) => void;
  activeProviders: Provider[];
  getCaps?: (model: string) => import("@/shared/constants/models").ModelCaps | null | undefined;
}) {
  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div className="min-w-0">
          <p className="text-sm font-medium">Vision Adapter</p>
          <p className="text-xs text-text-muted mt-0.5">
            Your model can&apos;t read image/audio? Auto-switches to a model in the pool below.
          </p>
          <ul className="mt-1.5 text-[11px] text-text-muted flex flex-col gap-0.5">
            <li><span className="font-medium text-text-main">Vision</span> — images (png, jpg, webp, …)</li>
            <li><span className="font-medium text-text-main">Audio</span> — audio input</li>
          </ul>
        </div>
      </div>
      <div className="flex flex-col gap-4">
        {CAPACITY_ADAPTER_CAPS.map((cap) => (
          <CapacityAdapterCap
            key={cap.key}
            cap={cap}
            entry={capacityAdapter[cap.key] || EMPTY_CAP_ENTRY}
            onChange={(entry) => onChange({ ...capacityAdapter, [cap.key]: entry })}
            activeProviders={activeProviders}
            getCaps={getCaps}
          />
        ))}
      </div>
    </div>
  );
}

function CapacityAdapterCap({
  cap,
  entry,
  onChange,
  activeProviders,
  getCaps,
}: {
  cap: { key: string; label: string; icon: string; desc: string };
  entry: CapacityAdapterEntry;
  onChange: (entry: CapacityAdapterEntry) => void;
  activeProviders: Provider[];
  getCaps?: (model: string) => import("@/shared/constants/models").ModelCaps | null | undefined;
}) {
  const [showModelSelect, setShowModelSelect] = useState(false);
  const { enabled, roundRobin, models } = entry;

  const patch = (p: Partial<CapacityAdapterEntry>) => onChange({ ...entry, ...p });

  const handleAdd = (model: { value: string }) => {
    if (models.includes(model.value)) return;
    patch({ models: [...models, model.value] });
  };

  const handleRemove = (index: number) => {
    const next = models.filter((_, i) => i !== index);
    patch({ models: next.length === 0 ? [DEFAULT_FALLBACK_MODEL] : next });
  };

  const handleMove = (index: number, delta: number) => {
    const target = index + delta;
    if (target < 0 || target >= models.length) return;
    const next = [...models];
    [next[index], next[target]] = [next[target], next[index]];
    patch({ models: next });
  };

  return (
    <Card padding="sm" className={`group ${!enabled ? "opacity-50" : ""}`}>
      <div className="flex min-w-0 flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        {/* Master toggle + icon + label + chips */}
        <div className="flex min-w-0 flex-1 items-start gap-2.5 sm:items-center">
          <Toggle
            checked={enabled}
            onChange={(v) => patch({ enabled: v })}
            aria-label={`Enable ${cap.label} adapter`}
          />
          <div className="size-8 rounded-lg bg-primary/10 flex items-center justify-center shrink-0">
            <span className="material-symbols-outlined text-primary text-[18px]">{cap.icon}</span>
          </div>
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-1.5">
              <code className="font-mono text-sm font-medium">{cap.label}</code>
              <span className="text-[10px] text-text-muted">— {cap.desc}</span>
            </div>
            <div className="mt-1 flex min-w-0 flex-wrap items-center gap-1">
              {models.length === 0 ? (
                <span className="text-xs text-text-muted italic">No models</span>
              ) : (
                models.slice(0, 3).map((model, index) => (
                  <code
                    key={`${model}-${index}`}
                    className="group/chip inline-flex items-center gap-1 rounded bg-black/5 px-1.5 py-0.5 font-mono text-xs text-text-muted dark:bg-white/5"
                  >
                    <span>{model}</span>
                    <CapacityBadges caps={getCaps?.(model)} />
                    <button onClick={() => handleMove(index, -1)} disabled={index === 0} className={`leading-none opacity-0 group-hover/chip:opacity-100 ${index === 0 ? "text-text-muted/20" : "text-text-muted hover:text-primary"}`}>
                      <span className="material-symbols-outlined text-[12px]">arrow_upward</span>
                    </button>
                    <button onClick={() => handleMove(index, 1)} disabled={index === models.length - 1} className={`leading-none opacity-0 group-hover/chip:opacity-100 ${index === models.length - 1 ? "text-text-muted/20" : "text-text-muted hover:text-primary"}`}>
                      <span className="material-symbols-outlined text-[12px]">arrow_downward</span>
                    </button>
                    <button onClick={() => handleRemove(index)} className="leading-none opacity-0 group-hover/chip:opacity-100 text-text-muted hover:text-red-500">
                      <span className="material-symbols-outlined text-[12px]">close</span>
                    </button>
                  </code>
                ))
              )}
              {models.length > 3 && (
                <span className="text-[10px] text-text-muted">+{models.length - 3} more</span>
              )}
            </div>
          </div>
        </div>

        {/* Actions: Round-robin toggle + Add Model */}
        <div className="flex w-full flex-col gap-2 sm:w-auto sm:flex-row sm:items-center sm:gap-3 sm:shrink-0">
          <label className="flex items-center gap-1.5 text-xs text-text-muted cursor-pointer select-none">
            <Toggle
              checked={roundRobin}
              onChange={(v) => patch({ roundRobin: v })}
              disabled={!enabled}
              aria-label={`Round-robin ${cap.label} adapter`}
            />
            <span>Round</span>
          </label>
          <Button
            icon="add"
            variant="ghost"
            size="sm"
            onClick={() => setShowModelSelect(true)}
            disabled={!enabled}
            title={`Add ${cap.label} model`}
          >
            Add Model
          </Button>
        </div>
      </div>

      {showModelSelect && (
        <ModelSelectModal
          isOpen={showModelSelect}
          onClose={() => setShowModelSelect(false)}
          onSelect={handleAdd}
          activeProviders={activeProviders}
          title={`Add ${cap.label} Model`}
          addedModelValues={models}
          capFilter={cap.key}
          closeOnSelect={false}
        />
      )}
    </Card>
  );
}
