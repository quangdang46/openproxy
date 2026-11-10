"use client";

import { useState, useEffect, useCallback } from "react";
import { Card, Button, Modal, Input, CardSkeleton, ModelSelectModal, SmartPolicyModal, Select, CapacityBadges, ComboFormModal, Toggle, Badge } from "@/shared/components";
import { ConfirmModal } from "@/shared/components/Modal";
import { useNotificationStore } from "@/store/notificationStore";
import { useSettingsStore } from "@/store/settingsStore";
import { useCopyToClipboard } from "@/shared/hooks/useCopyToClipboard";
import { useModelCaps } from "@/shared/hooks/useModelCaps";
import { isOpenAICompatibleProvider, isAnthropicCompatibleProvider, resolveProviderId, AI_PROVIDERS } from "@/shared/constants/providers";
import {
  addPoolModel,
  deselectPoolModel,
  removePoolModelAt,
} from "@/shared/utils/capacityAdapterPool";
import { fmtK } from "@/shared/utils/comboCaps";
import type { ComboStrategyConfig, ComboStrategyOption } from "@/types";

const STRATEGY_OPTIONS: ComboStrategyOption[] = [
  { value: "fallback", label: "Fallback — try in order" },
  { value: "round-robin", label: "Round Robin — rotate" },
  { value: "smart", label: "Smart Router — System One" },
  { value: "fusion", label: "Fusion — panel + judge" },
  { value: "cheapest", label: "Cheapest — free/lowest cost first" },
  { value: "fastest", label: "Fastest — lowest latency first" },
  { value: "quality", label: "Quality — capability tier first" },
];

export type { Combo, ComboFormProvider, ComboHealthEntry } from "@/shared/components/ComboFormModal";
import type {
  Combo,
  ComboFormProvider as Provider,
  ComboHealthEntry,
} from "@/shared/components/ComboFormModal";

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
// 9router DEFAULT_FALLBACK_MODEL — v2.6 free tier (capacityAdapter.js:15);
// legacy oc/mimo-v2.5-free pool entries are upgraded by the backend
// (capacity_adapter.rs `upgrade_legacy_model`) and by normalizeCapEntry below.
const DEFAULT_FALLBACK_MODEL = "oc/mimo-v2.6-flash-free";
const EMPTY_CAP_ENTRY: CapacityAdapterEntry = { enabled: true, roundRobin: false, models: [] };
const EMPTY_CAPACITY_ADAPTER: Record<string, CapacityAdapterEntry> = {
  vision: { ...EMPTY_CAP_ENTRY },
  pdf: { ...EMPTY_CAP_ENTRY },
  audioInput: { ...EMPTY_CAP_ENTRY },
  videoInput: { ...EMPTY_CAP_ENTRY },
};
// 9router upgradeLegacyModel (combos/page.js:33): a stored legacy pool model
// resolves to the current default.
function upgradeLegacyModel(model: string): string {
  return model === "oc/mimo-v2.5-free" ? DEFAULT_FALLBACK_MODEL : model;
}

// Backward-compat: legacy stored form was an array of {model, enabled}.
function normalizeCapEntry(entry: any): CapacityAdapterEntry {
  if (Array.isArray(entry)) {
    return {
      enabled: true,
      roundRobin: false,
      models: entry
        .map((e: any) => upgradeLegacyModel(e?.model || e))
        .filter(Boolean),
    };
  }
  if (entry && typeof entry === "object") {
    return {
      enabled: entry.enabled !== false,
      roundRobin: !!entry.roundRobin,
      models: Array.isArray(entry.models)
        ? entry.models.map(upgradeLegacyModel).filter(Boolean)
        : [],
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

/** Policy badge label — 9router combos/page.js getPolicyBadgeLabel. */
function getPolicyBadgeLabel(strategy: ComboStrategyConfig = {}): string {
  const policy =
    strategy.smartPolicy ||
    (strategy.sessionSticky === false ? "per-turn" : "smart");
  if (policy === "strict") return "Strict Lock";
  if (policy === "per-turn") return "Per-Turn";
  if (policy === "custom") {
    const raw = strategy.confidenceThreshold;
    const pct = Math.round((raw || 0.8) * (raw && raw <= 1 ? 100 : 1));
    return `Custom (≥${pct || 80}%)`;
  }
  return "Smart (≥80%)";
}

/** Router credential status — 9router combos/page.js getRouterModelStatus. */
function getRouterModelStatus(
  routerModel: string | undefined,
  activeProviders: { provider: string; isActive?: boolean }[],
): { ready: boolean; label: string; providerId: string } {
  const effectiveModel =
    typeof routerModel === "string" && routerModel.trim()
      ? routerModel.trim()
      : "oc/jev-1.13-free";
  const slashIdx = effectiveModel.indexOf("/");
  const prefix = slashIdx !== -1 ? effectiveModel.slice(0, slashIdx) : effectiveModel;
  const providerId = resolveProviderId(prefix);
  const providerDef = AI_PROVIDERS[providerId];
  const isNoAuth = !!providerDef?.noAuth;

  if (isNoAuth) {
    return { ready: true, label: "Ready (Free)", providerId };
  }

  const isConnected = (activeProviders || []).some(
    (c) => c.provider === providerId && c.isActive !== false,
  );
  if (isConnected) {
    return { ready: true, label: "Connected", providerId };
  }

  return { ready: false, label: "Not configured", providerId };
}

export default function CombosPage() {
  const [combos, setCombos] = useState<Combo[]>([]);
  const [loading, setLoading] = useState<boolean>(true);
  const [showCreateModal, setShowCreateModal] = useState<boolean>(false);
  const [editingCombo, setEditingCombo] = useState<Combo | null>(null);
  const [activeProviders, setActiveProviders] = useState<Provider[]>([]);
  const [comboStrategies, setComboStrategies] = useState<Record<string, any>>({});
  // False when the settings read failed. The strategy PATCH below is a
  // read-modify-write over this map, so writing from an unseeded one would
  // PATCH {} over every other combo strategy.
  const [comboStrategiesLoaded, setComboStrategiesLoaded] = useState<boolean>(false);
  const [deleteTarget, setDeleteTarget] = useState<Combo | null>(null);
  const [deleting, setDeleting] = useState<boolean>(false);
  // Bulk selection + search (9router combos/page.js parity).
  const [selectedIds, setSelectedIds] = useState<string[]>([]);
  const [bulkBusy, setBulkBusy] = useState<boolean>(false);
  const [searchQuery, setSearchQuery] = useState<string>("");
  const [presetLoading, setPresetLoading] = useState<"cursor" | "claude" | null>(null);
  const [presetConfirm, setPresetConfirm] = useState<{
    source: "cursor" | "claude";
    toCreate: number;
    toSkip: number;
    total: number;
  } | null>(null);
  const [bulkDeleteOpen, setBulkDeleteOpen] = useState<boolean>(false);
  const [bulkDeleteLoading, setBulkDeleteLoading] = useState<boolean>(false);
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

  // Drop stale selection when the combo list changes (delete / refresh).
  useEffect(() => {
    const alive = new Set(combos.map((c) => c.id));
    setSelectedIds((prev) => prev.filter((id) => alive.has(id)));
  }, [combos]);

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
      setComboStrategiesLoaded(Boolean(settingsData));
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

  // Member order IS the fallback priority, so the card reorders in place the
  // same way the capacity-adapter pools do.
  const handleReorderModels = async (combo: Combo, from: number, to: number) => {
    if (to < 0 || to >= combo.models.length || from === to) return;
    const models = [...combo.models];
    [models[from], models[to]] = [models[to], models[from]];
    try {
      const res = await fetch(`/api/combos/${combo.id}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: combo.name,
          models,
          disabledModels: combo.disabledModels || [],
        }),
      });
      if (res.ok) {
        await fetchData();
      } else {
        const err = await res.json();
        notify.error(err.error || "Failed to reorder combo");
      }
    } catch (error) {
      notify.error("Failed to reorder combo");
      console.log("Error reordering combo:", error);
    }
  };

  const confirmDelete = async () => {
    if (!deleteTarget) return;
    setDeleting(true);
    try {
      const res = await fetch(`/api/combos/${deleteTarget.id}`, { method: "DELETE" });
      if (res.ok) {
        await pruneStrategiesForNames([deleteTarget.name]);
        setCombos(combos.filter(c => c.id !== deleteTarget.id));
        setSelectedIds((prev) => prev.filter((x) => x !== deleteTarget.id));
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
    if (!comboStrategiesLoaded) {
      // The settings read failed, so this map is empty because we know
      // nothing, not because there is nothing. Writing it back would PATCH an
      // empty comboStrategies over every other combo's setting.
      console.warn("Refusing to write comboStrategies: settings were never loaded");
      return;
    }
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

  // ── Bulk selection / search (9router combos/page.js:81-122) ──
  const selectedCombos = combos.filter((c) => selectedIds.includes(c.id));
  const someSelected = selectedIds.length > 0;

  // Case-insensitive search over the combo name and its model ids.
  const normalizedQuery = searchQuery.trim().toLowerCase();
  const filteredCombos = normalizedQuery
    ? combos.filter((c) => {
        if (String(c.name || "").toLowerCase().includes(normalizedQuery)) return true;
        return (c.models || []).some((m: string) => String(m).toLowerCase().includes(normalizedQuery));
      })
    : combos;

  // Select-all follows the search: with a query active it must only reach the
  // rows on screen, otherwise bulk Delete would take out hidden combos.
  const allSelected =
    filteredCombos.length > 0 && filteredCombos.every((c) => selectedIds.includes(c.id));

  const toggleSelect = (id: string) => {
    setSelectedIds((prev) =>
      prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id],
    );
  };

  const toggleSelectAll = () => {
    setSelectedIds(allSelected ? [] : filteredCombos.map((c) => c.id));
  };

  const clearSelection = () => setSelectedIds([]);

  // ── Combo presets (9router /api/combos/presets + Cursor/Claude Default) ──
  const handleGeneratePresets = async (source: "cursor" | "claude") => {
    const label = source === "cursor" ? "Cursor Default" : "Claude Default";
    setPresetLoading(source);
    try {
      const previewRes = await fetch(`/api/combos/presets?source=${source}`);
      const preview = await previewRes.json();
      if (!previewRes.ok) {
        notify.error(preview.error || `Failed to preview ${label}`);
        return;
      }

      const items: { name: string; exists: boolean }[] = preview.items || [];
      const toCreate =
        preview.toCreate ?? items.filter((i) => !i.exists).length;
      const toSkip = preview.toSkip ?? items.filter((i) => i.exists).length;
      const total = items.length;

      if (total === 0) {
        notify.info(`No ${label} models available to generate.`);
        return;
      }
      if (toCreate === 0) {
        notify.info(`All ${total} ${label} combos already exist. Nothing to create.`);
        return;
      }

      setPresetConfirm({ source, toCreate, toSkip, total });
    } catch (error) {
      console.log(`Error previewing ${label}:`, error);
      notify.error(`Failed to preview ${label}`);
    } finally {
      setPresetLoading(null);
    }
  };

  const confirmGeneratePresets = async () => {
    if (!presetConfirm) return;
    const { source, toCreate, toSkip } = presetConfirm;
    const label = source === "cursor" ? "Cursor Default" : "Claude Default";
    try {
      const res = await fetch("/api/combos/presets", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ source }),
      });
      const data = await res.json();
      if (!res.ok) {
        notify.error(data.error || `Failed to generate ${label}`);
        return;
      }
      notify.success(
        `Created ${data.createdCount ?? toCreate} ${label} combo${(data.createdCount ?? toCreate) === 1 ? "" : "s"}` +
          (toSkip > 0 ? ` (${toSkip} already existed)` : ""),
      );
      await fetchData();
      setPresetConfirm(null);
    } catch (error) {
      console.log(`Error generating ${label}:`, error);
      notify.error(`Failed to generate ${label}`);
    }
  };

  // Drop per-combo strategy entries for deleted names so settings stay clean.
  const pruneStrategiesForNames = async (names: string[]) => {
    if (!comboStrategiesLoaded || names.length === 0) return;
    const updated = { ...comboStrategies };
    for (const name of names) delete updated[name];
    try {
      await fetch("/api/settings", {
        method: "PATCH",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ comboStrategies: updated }),
      });
      setComboStrategies(updated);
    } catch (error) {
      console.log("Error pruning combo strategies:", error);
    }
  };

  const confirmBulkDelete = async () => {
    if (selectedCombos.length === 0) return;
    setBulkDeleteLoading(true);
    try {
      const ids = selectedCombos.map((c) => c.id);
      const names = selectedCombos.map((c) => c.name);
      const results = await Promise.all(
        ids.map((id) => fetch(`/api/combos/${id}`, { method: "DELETE" })),
      );
      const failed = results.filter((r) => !r.ok).length;
      await pruneStrategiesForNames(names);
      setCombos((prev) => prev.filter((c) => !ids.includes(c.id)));
      clearSelection();
      setBulkDeleteOpen(false);
      if (failed > 0) {
        notify.error(`Deleted with ${failed} failure${failed === 1 ? "" : "s"}.`);
      } else {
        notify.success(`Deleted ${ids.length} combo${ids.length === 1 ? "" : "s"}`);
      }
    } catch (error) {
      console.log("Error bulk deleting combos:", error);
      notify.error("Failed to delete selected combos");
    } finally {
      setBulkDeleteLoading(false);
    }
  };

  const handleBulkSetStrategy = async (strategy: string) => {
    if (selectedCombos.length === 0 || !strategy) return;
    if (!comboStrategiesLoaded) {
      console.warn("Refusing to write comboStrategies: settings were never loaded");
      return;
    }
    setBulkBusy(true);
    try {
      const updated = { ...comboStrategies };
      for (const combo of selectedCombos) {
        if (strategy === "fallback") {
          delete updated[combo.name];
        } else {
          updated[combo.name] = {
            ...normalizeStrategy(updated[combo.name]),
            fallbackStrategy: strategy,
          };
        }
      }
      await fetch("/api/settings", {
        method: "PATCH",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ comboStrategies: updated }),
      });
      setComboStrategies(updated);
      notify.success(
        `Strategy updated for ${selectedCombos.length} combo${selectedCombos.length === 1 ? "" : "s"}`,
      );
    } catch (error) {
      console.log("Error bulk updating combo strategy:", error);
      notify.error("Failed to update strategy for selected combos");
    } finally {
      setBulkBusy(false);
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
              <span className="font-medium text-text-main">Smart Router</span> — queries a System One
              decision model to pick the optimal model before generation; supports Smart Switch
              (adaptive hysteresis), Strict Lock (1 task = 1 session, +0ms latency), and Per-Turn
              modes to maximize prompt-cache retention
            </li>
            <li>
              <span className="font-medium text-text-main">Fusion</span> — queries all models in
              parallel, then a judge synthesizes one answer. Best quality, but costs the most: every
              request bills all panel models + the judge (N+1 calls)
            </li>
          </ul>
        </div>
        <div className="flex w-full flex-col gap-2 sm:w-auto sm:items-stretch">
          <div className="relative w-full sm:w-64">
            <span className="material-symbols-outlined pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-[18px] text-text-muted">
              search
            </span>
            <input
              type="text"
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              placeholder="Search combos…"
              aria-label="Search combos"
              className="w-full rounded-[10px] border border-transparent bg-surface-2 py-2 pl-10 pr-9 text-sm text-text-main transition-all duration-150 ease-out placeholder-text-muted/70 focus:border-brand-500/40 focus:outline-none focus:ring-2 focus:ring-brand-500/30"
            />
            {searchQuery && (
              <button
                type="button"
                onClick={() => setSearchQuery("")}
                aria-label="Clear search"
                className="absolute right-2 top-1/2 -translate-y-1/2 rounded p-1 text-text-muted transition-colors hover:text-text-main"
              >
                <span className="material-symbols-outlined text-[16px]">close</span>
              </button>
            )}
          </div>
          <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
            <Button icon="add" onClick={() => setShowCreateModal(true)} className="w-full sm:w-auto whitespace-nowrap">
              Create Combo
            </Button>
            <Button
              variant="secondary"
              size="sm"
              icon="edit_note"
              loading={presetLoading === "cursor"}
              disabled={!!presetLoading}
              onClick={() => handleGeneratePresets("cursor")}
              className="w-full sm:w-auto whitespace-nowrap"
            >
              Cursor Default
            </Button>
            <Button
              variant="secondary"
              size="sm"
              icon="smart_toy"
              loading={presetLoading === "claude"}
              disabled={!!presetLoading}
              onClick={() => handleGeneratePresets("claude")}
              className="w-full sm:w-auto whitespace-nowrap"
            >
              Claude Default
            </Button>
          </div>
        </div>
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
        <div className="flex flex-col gap-3">
          {/* Selection toolbar */}
          <div className="flex min-w-0 flex-col gap-2 rounded-lg border border-black/5 bg-black/[0.015] px-3 py-2 dark:border-white/5 dark:bg-white/[0.02] sm:flex-row sm:items-center sm:justify-between">
            <label className="flex cursor-pointer items-center gap-2 text-xs text-text-muted hover:text-primary select-none">
              <input
                type="checkbox"
                checked={allSelected}
                ref={(el) => {
                  if (el) el.indeterminate = someSelected && !allSelected;
                }}
                onChange={toggleSelectAll}
                className="h-3.5 w-3.5 rounded border-gray-300 text-primary focus:ring-primary"
              />
              <span>
                {someSelected
                  ? `${selectedIds.length} selected`
                  : `Select all (${filteredCombos.length})`}
              </span>
            </label>

            <div className="flex min-w-0 flex-wrap items-center gap-2">
              {normalizedQuery && (
                <span className="text-xs text-text-muted">
                  {filteredCombos.length} of {combos.length} shown
                </span>
              )}
              {someSelected && (
                <>
                  <div className="w-full min-w-[160px] sm:w-[200px]">
                    <Select
                      options={STRATEGY_OPTIONS}
                      value=""
                      placeholder="Set strategy…"
                      disabled={bulkBusy}
                      onChange={(e) => {
                        const v = e.target.value;
                        if (v) handleBulkSetStrategy(v);
                      }}
                      selectClassName="py-1.5 text-xs"
                    />
                  </div>
                  <Button
                    size="sm"
                    variant="danger"
                    icon="delete"
                    disabled={bulkBusy}
                    loading={bulkBusy}
                    onClick={() => setBulkDeleteOpen(true)}
                    className="whitespace-nowrap"
                  >
                    Delete ({selectedIds.length})
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={clearSelection}
                    disabled={bulkBusy}
                  >
                    Clear
                  </Button>
                </>
              )}
            </div>
          </div>

          {filteredCombos.length === 0 ? (
            <Card>
              <div className="text-center py-10">
                <p className="text-text-main font-medium mb-1">
                  No combos match &ldquo;{searchQuery.trim()}&rdquo;
                </p>
                <p className="text-sm text-text-muted mb-4">
                  Try a shorter query, or clear the search to see all {combos.length} combos.
                </p>
                <Button variant="secondary" onClick={() => setSearchQuery("")}>
                  Clear search
                </Button>
              </div>
            </Card>
          ) : (
            <div className="flex flex-col gap-4">
              {filteredCombos.map((combo) => (
                <ComboCard
                  key={combo.id}
                  combo={combo}
                  activeProviders={activeProviders}
                  copied={copied}
                  onCopy={copy}
                  onEdit={() => setEditingCombo(combo)}
                  onDelete={() => handleDelete(combo)}
                  onReorder={(from, to) => handleReorderModels(combo, from, to)}
                  strategy={normalizeStrategy(comboStrategies[combo.name])}
                  onSetStrategy={(patch) => handleSetComboStrategy(combo.name, patch)}
                  getCaps={getCaps}
                  selected={selectedIds.includes(combo.id)}
                  onToggleSelect={() => toggleSelect(combo.id)}
                />
              ))}
            </div>
          )}
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

      {/* Bulk delete selected combos */}
      <ConfirmModal
        isOpen={bulkDeleteOpen}
        onClose={() => !bulkDeleteLoading && setBulkDeleteOpen(false)}
        onConfirm={confirmBulkDelete}
        title="Delete Selected Combos"
        message={
          <>
            Delete {selectedCombos.length} selected combo{selectedCombos.length === 1 ? "" : "s"}? This cannot be undone.
          </>
        }
        confirmText="Delete"
        variant="danger"
        loading={bulkDeleteLoading}
      />

      {/* Generate Cursor/Claude default presets */}
      <ConfirmModal
        isOpen={!!presetConfirm}
        onClose={() => setPresetConfirm(null)}
        onConfirm={confirmGeneratePresets}
        title={presetConfirm ? `Generate ${presetConfirm.source === "cursor" ? "Cursor Default" : "Claude Default"}` : "Confirm"}
        message={
          presetConfirm ? (
            <>
              Create {presetConfirm.toCreate} combo{presetConfirm.toCreate === 1 ? "" : "s"} named like{" "}
              {presetConfirm.source === "cursor" ? "Cursor" : "Claude"} model IDs (seeded with{" "}
              <code>cu/…</code> or <code>cc/…</code>). {presetConfirm.toSkip} already exist
              and will be skipped. You can edit any combo afterward to add fallbacks.
            </>
          ) : null
        }
        confirmText="Generate"
        variant="primary"
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
  onReorder: (from: number, to: number) => void;
  strategy: ComboStrategyConfig;
  onSetStrategy: (patch: Partial<ComboStrategyConfig>) => void;
  getCaps?: (model: string) => import("@/shared/constants/models").ModelCaps | null | undefined;
  selected?: boolean;
  onToggleSelect?: () => void;
}

function ComboCard({
  combo,
  activeProviders,
  copied,
  onCopy,
  onEdit,
  onDelete,
  onReorder,
  strategy,
  onSetStrategy,
  getCaps,
  selected = false,
  onToggleSelect,
}: ComboCardProps) {
  const [health, setHealth] = useState<ComboHealthEntry[]>([]);
  const [showJudgeSelect, setShowJudgeSelect] = useState(false);
  const [showRouterSelect, setShowRouterSelect] = useState(false);
  const [showPolicyModal, setShowPolicyModal] = useState(false);

  const current = strategy.fallbackStrategy || "fallback";
  const judge = strategy.judgeModel || "";
  const routerModel = strategy.routerModel || "";
  const isFusion = current === "fusion";
  const isSmart = current === "smart";

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
    <Card padding="sm" className={`group ${selected ? "ring-1 ring-primary/40 bg-primary/[0.03]" : ""}`}>
      <div className="flex min-w-0 flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex min-w-0 flex-1 items-start gap-3 sm:items-center">
          {onToggleSelect && (
            <label className="flex shrink-0 items-center pt-1 sm:pt-0 cursor-pointer" title="Select combo">
              <input
                type="checkbox"
                checked={selected}
                onChange={onToggleSelect}
                onClick={(e) => e.stopPropagation()}
                className="h-4 w-4 rounded border-gray-300 text-primary focus:ring-primary"
                aria-label={`Select ${combo.name}`}
              />
            </label>
          )}
          <div className="size-8 rounded-lg bg-primary/10 flex items-center justify-center shrink-0">
            <span className="material-symbols-outlined text-primary text-[18px]">layers</span>
          </div>
          <div className="min-w-0 flex-1">
            <code className="block truncate font-mono text-sm font-medium">{combo.name}</code>
            <div className="mt-1 flex min-w-0 flex-wrap items-center gap-1">
              {combo.models.length === 0 ? (
                <span className="text-xs text-text-muted italic">No models</span>
              ) : (
                combo.models.map((model, index) => {
                  const isDisabled = disabled.includes(model);
                  const isQuarantined = quarantined.some((q) => q.model === model);
                  const isFirst = index === 0;
                  const isLast = index === combo.models.length - 1;
                  return (
                    <code
                      key={`${model}-${index}`}
                      className={`inline-flex max-w-full items-center gap-1 rounded px-1.5 py-0.5 font-mono text-[10px] sm:max-w-[220px] ${
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
                            : "Fallback priority — higher is tried first"
                      }
                    >
                      <span className="truncate">{model}</span>
                      {getCaps && <CapacityBadges caps={getCaps(model)} size={11} colorOverride="text-text-muted/70" />}
                      <button onClick={() => onReorder(index, index - 1)} disabled={isFirst} className={`leading-none ${isFirst ? "text-text-muted/20" : "text-text-muted hover:text-primary"}`} title="Move up">
                        <span className="material-symbols-outlined text-[12px]">arrow_upward</span>
                      </button>
                      <button onClick={() => onReorder(index, index + 1)} disabled={isLast} className={`leading-none ${isLast ? "text-text-muted/20" : "text-text-muted hover:text-primary"}`} title="Move down">
                        <span className="material-symbols-outlined text-[12px]">arrow_downward</span>
                      </button>
                    </code>
                  );
                })
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
            {/* Aggregated capability envelope — the backend computes
                aggregate_combo_capabilities per combo (9router 5c217d3);
                absent when the combo has no resolvable members. */}
            {combo.capabilities && (
              <div className="mt-1 flex items-center gap-2 text-[10px] text-text-muted">
                <span>ctx {fmtK(combo.capabilities.contextWindow)}</span>
                <span className="opacity-40">·</span>
                <span>max {fmtK(combo.capabilities.maxOutput)}</span>
              </div>
            )}
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
            {/* Smart: System One router picker and policy modal trigger
                (9router combos/page.js ComboCard isSmart block). */}
            {isSmart && (() => {
              const routerStatus = getRouterModelStatus(routerModel, activeProviders);
              const policyLabel = getPolicyBadgeLabel(strategy);
              return (
                <div className="mt-2 flex min-w-0 flex-wrap items-center gap-1.5">
                  <span className="text-[11px] font-medium text-text-muted">Router</span>
                  <button
                    type="button"
                    onClick={() => setShowRouterSelect(true)}
                    className="inline-flex max-w-full items-center gap-1 rounded border border-dashed border-primary/40 px-1.5 py-0.5 font-mono text-[11px] text-primary hover:border-primary hover:bg-primary/5 transition-colors"
                    title="Pick the System One model that decides model routing"
                  >
                    <span className="material-symbols-outlined text-[13px]">psychology</span>
                    <span className="truncate">
                      {routerModel || "Auto — oc/jev-1.13-free"}
                    </span>
                  </button>
                  {routerStatus.ready ? (
                    <Badge variant="success" size="sm" dot>{routerStatus.label}</Badge>
                  ) : (
                    <span className="inline-flex items-center gap-1">
                      <Badge variant="error" size="sm" dot>{routerStatus.label}</Badge>
                      <a
                        href={`/dashboard/media-providers/systemone/${routerStatus.providerId}`}
                        className="text-[10px] text-primary hover:underline font-medium"
                        title="Configure in Media Providers"
                      >
                        Configure
                      </a>
                    </span>
                  )}
                  <button
                    type="button"
                    onClick={() => setShowPolicyModal(true)}
                    className="inline-flex items-center gap-1 rounded border border-border/70 bg-black/[0.02] dark:bg-white/[0.02] px-2 py-0.5 text-[11px] text-text-main hover:border-primary/50 hover:bg-primary/5 transition-colors"
                    title="Configure routing policy and prompt cache retention"
                  >
                    <span className="material-symbols-outlined text-[13px] text-text-muted">tune</span>
                    <span className="font-medium">{policyLabel}</span>
                  </button>
                  {routerModel && (
                    <button
                      type="button"
                      onClick={() => onSetStrategy({ routerModel: "" })}
                      className="p-0.5 rounded text-text-muted hover:text-red-500 hover:bg-red-500/10 transition-colors"
                      title="Reset router to Auto"
                    >
                      <span className="material-symbols-outlined text-[13px]">close</span>
                    </button>
                  )}
                </div>
              );
            })()}
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

      {/* System One Router model picker (single-select from systemone kind) */}
      <ModelSelectModal
        isOpen={showRouterSelect}
        onClose={() => setShowRouterSelect(false)}
        onSelect={(m) => {
          onSetStrategy({ routerModel: m?.value || "" });
          setShowRouterSelect(false);
        }}
        activeProviders={activeProviders}
        title="Select System One Router Model"
        kindFilter="systemone"
        addedModelValues={routerModel ? [routerModel] : []}
        closeOnSelect={true}
      />

      {/* Smart Router Policy Settings Modal */}
      <SmartPolicyModal
        isOpen={showPolicyModal}
        onClose={() => setShowPolicyModal(false)}
        comboName={combo.name}
        currentPolicy={
          strategy.smartPolicy ||
          (strategy.sessionSticky === false ? "per-turn" : "smart")
        }
        currentThreshold={strategy.confidenceThreshold ?? 80}
        onSave={onSetStrategy}
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

  // Pool mutations live in shared/utils/capacityAdapterPool.ts (unit-tested in
  // web/tests/capacityAdapterPool.test.ts) — 9router commit 3e4323e semantics:
  // the picker may hand back a Model object or a bare string, additions dedupe,
  // and every removal falls back to the default when the pool would empty.
  const handleAdd = (model: unknown) => {
    patch({ models: addPoolModel(models, model) });
  };

  // Deselect from the model picker removes the entry from the ordered pool.
  const handleDeselect = (model: unknown) => {
    patch({ models: deselectPoolModel(models, model, DEFAULT_FALLBACK_MODEL) });
  };

  const handleRemove = (index: number) => {
    patch({ models: removePoolModelAt(models, index, DEFAULT_FALLBACK_MODEL) });
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

      {/* Ordered model pool — 9router commit 3e4323e table view replaces the
          old first-3 chips; every member is listed with its fallback order. */}
      {models.length === 0 ? (
        <div className="mt-3 py-2 text-center text-xs text-text-muted italic">
          No models in pool (will fallback to {DEFAULT_FALLBACK_MODEL})
        </div>
      ) : (
        <div className="mt-3 overflow-hidden rounded-lg border border-border/50">
          <table className="w-full text-left text-xs">
            <thead>
              <tr className="border-b border-border/40 bg-black/[0.02] text-text-muted dark:bg-white/[0.02]">
                <th className="w-12 px-3 py-1.5 font-medium text-center">#</th>
                <th className="px-3 py-1.5 font-medium">Model</th>
                <th className="w-24 px-3 py-1.5 font-medium text-center">Order</th>
                <th className="w-12 px-3 py-1.5 font-medium text-right"></th>
              </tr>
            </thead>
            <tbody className="divide-y divide-border/30 font-mono">
              {models.map((model, index) => (
                <tr
                  key={`${model}-${index}`}
                  className="transition-colors hover:bg-black/[0.02] dark:hover:bg-white/[0.02]"
                >
                  <td className="px-3 py-2 text-center font-sans text-[11px] text-text-muted">
                    #{index + 1}
                  </td>
                  <td className="px-3 py-2 text-text-main">
                    <div className="flex flex-wrap items-center gap-1.5">
                      <span className="truncate">{model}</span>
                      <CapacityBadges caps={getCaps?.(model)} />
                      {model === DEFAULT_FALLBACK_MODEL && (
                        <span className="rounded bg-emerald-500/10 px-1.5 py-0.5 font-sans text-[10px] font-medium text-emerald-600 dark:text-emerald-400">
                          free default
                        </span>
                      )}
                    </div>
                  </td>
                  <td className="px-3 py-2 text-center">
                    <div className="inline-flex items-center gap-1">
                      <button
                        type="button"
                        onClick={() => handleMove(index, -1)}
                        disabled={!enabled || index === 0}
                        className={`rounded p-1 transition-colors ${
                          !enabled || index === 0
                            ? "cursor-not-allowed text-text-muted/20"
                            : "text-text-muted hover:bg-black/5 hover:text-primary dark:hover:bg-white/5"
                        }`}
                        title="Move up"
                      >
                        <span className="material-symbols-outlined text-[16px] leading-none">
                          arrow_upward
                        </span>
                      </button>
                      <button
                        type="button"
                        onClick={() => handleMove(index, 1)}
                        disabled={!enabled || index === models.length - 1}
                        className={`rounded p-1 transition-colors ${
                          !enabled || index === models.length - 1
                            ? "cursor-not-allowed text-text-muted/20"
                            : "text-text-muted hover:bg-black/5 hover:text-primary dark:hover:bg-white/5"
                        }`}
                        title="Move down"
                      >
                        <span className="material-symbols-outlined text-[16px] leading-none">
                          arrow_downward
                        </span>
                      </button>
                    </div>
                  </td>
                  <td className="px-3 py-2 text-right">
                    <button
                      type="button"
                      onClick={() => handleRemove(index)}
                      disabled={!enabled}
                      className={`rounded p-1 transition-colors ${
                        !enabled
                          ? "cursor-not-allowed text-text-muted/20"
                          : "text-text-muted hover:bg-red-500/10 hover:text-red-500"
                      }`}
                      title="Remove model"
                    >
                      <span className="material-symbols-outlined text-[16px] leading-none">
                        close
                      </span>
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {showModelSelect && (
        <ModelSelectModal
          isOpen={showModelSelect}
          onClose={() => setShowModelSelect(false)}
          onSelect={handleAdd}
          onDeselect={handleDeselect}
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
