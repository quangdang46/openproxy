"use client";

import { useState, useMemo, useEffect, useRef } from "react";
import Modal from "./Modal";
import Button from "./Button";
import CapacityBadges from "./CapacityBadges";
import { useModelCaps } from "@/shared/hooks/useModelCaps";
import { getModelsByProviderId, useEnsureCatalog } from "@/shared/constants/models";
import { OAUTH_PROVIDERS, APIKEY_PROVIDERS, FREE_PROVIDERS, FREE_TIER_PROVIDERS, AI_PROVIDERS, isOpenAICompatibleProvider, isAnthropicCompatibleProvider, getProviderAlias } from "@/shared/constants/providers";
import { getProviderCustomModelRows } from "@/shared/utils/providerCustomModels";
import { buildAvailableModels, fetchLiveModels, loadFreeOnly, useFavorites, type LiveModel } from "@/shared/models/availableModels";
import React from "react";

interface Model {
  id: string;
  name: string;
  value: string;
  type?: string;
  isPlaceholder?: boolean;
  isCustom?: boolean;
}

interface ModelGroup {
  name: string;
  alias: string;
  color: string;
  models: Model[];
  isCustom?: boolean;
  hasModels?: boolean;
}

interface ActiveProvider {
  provider: string;
  [key: string]: any;
}

interface Combo {
  id: string;
  name: string;
}

interface ProviderNode {
  id: string;
  name?: string;
  prefix?: string;
}

interface CustomModel {
  id: string;
  name?: string;
  providerAlias: string;
}

// Provider order: OAuth first, then Free Tier, then API Key (matches dashboard/providers)
const PROVIDER_ORDER = [
  ...Object.keys(OAUTH_PROVIDERS),
  ...Object.keys(FREE_PROVIDERS),
  ...Object.keys(FREE_TIER_PROVIDERS),
  ...Object.keys(APIKEY_PROVIDERS),
];

// Providers that need no auth — always show in model selector
const NO_AUTH_PROVIDER_IDS = Object.keys(FREE_PROVIDERS).filter(id => FREE_PROVIDERS[id].noAuth);

interface ModelSelectModalProps {
  isOpen: boolean;
  onClose: () => void;
  onSelect: (model: Model) => void;
  selectedModel?: string | string[];
  activeProviders?: ActiveProvider[];
  title?: string;
  modelAliases?: Record<string, string>;
  kindFilter?: string | null;
  /** Filter models by input-modality capability (vision/pdf/audioInput/videoInput). 9router parity. */
  capFilter?: string | null;
  /** Values already added — floated to top of sort. 9router parity. */
  addedModelValues?: string[];
  // When false, picking a model does not close the modal; the user must press
  // Done. Useful when the parent uses onSelect to toggle multiple entries.
  closeOnSelect?: boolean;
  // Multi-select mode: rows render a checkbox, clicking a row toggles
  // selection (instead of opening), and the footer shows "Apply N".
  selectionMode?: "single" | "multi";
  // Called with the selected model ids when the user presses "Apply N".
  onSelectIds?: (ids: string[]) => void;
}

export default function ModelSelectModal({
  isOpen,
  onClose,
  onSelect,
  selectedModel,
  activeProviders = [],
  title = "Select Model",
  modelAliases = {},
  kindFilter = null,
  capFilter = null,
  addedModelValues = [],
  closeOnSelect = true,
  selectionMode = "single",
  onSelectIds,
}: ModelSelectModalProps) {
  // `catalogReady` gates the grouping memo: without it as a dependency the
  // memo runs once against an empty catalog and the modal opens showing zero
  // models with no way to recover.
  const catalogReady = useEnsureCatalog();
  const { getCaps } = useModelCaps();
  // Filter activeProviders by serviceKinds when kindFilter set (e.g. "webSearch", "webFetch")
  const filteredActiveProviders = useMemo(() => {
    if (!kindFilter) return activeProviders;
    return activeProviders.filter((p) => {
      const info = AI_PROVIDERS[p.provider];
      const kinds = info?.serviceKinds || ["llm"];
      return kinds.includes(kindFilter);
    });
  }, [activeProviders, kindFilter]);
  const [searchQuery, setSearchQuery] = useState("");
  // Fallback source for model aliases. Several tool pickers (ClineToolCard,
  // DefaultToolCard, KiloToolCard) never pass the `modelAliases` prop, and for
  // compatible providers it is the ONLY source of real models — an empty prop
  // used to degrade the list to a literal placeholder row.
  const [fetchedAliases, setFetchedAliases] = useState<Record<string, string>>({});
  const [combos, setCombos] = useState<Combo[]>([]);
  const [providerNodes, setProviderNodes] = useState<ProviderNode[]>([]);
  const [customModels, setCustomModels] = useState<CustomModel[]>([]);
  const [disabledMap, setDisabledMap] = useState<Record<string, string[]>>({});
  const [liveModelsByAlias, setLiveModelsByAlias] = useState<Record<string, LiveModel[]>>({});
  const [freeOnlyByAlias, setFreeOnlyByAlias] = useState<Record<string, boolean>>({});
  // Simulation mock badges (bead sim-20): same /api/mock/status source as
  // the provider page toggle (AGENTS.md consistency rule).
  const [mockByAlias, setMockByAlias] = useState<Record<string, boolean>>({});
  const listRef = useRef<HTMLDivElement>(null);

  // Shared favorites (star) store — same cache the provider page uses.
  const { isFavorite, toggleFavorite } = useFavorites();

  // Multi-select state (modal-scoped; the modal is the single owner since it
  // spans many providers at once).
  const [selectedIds, setSelectedIds] = useState<string[]>([]);
  const toggleSelect = (id: string) =>
    setSelectedIds((prev) =>
      prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id]
    );
  useEffect(() => {
    if (isOpen) setSelectedIds([]);
  }, [isOpen]);

  const fetchCombos = async () => {
    try {
      const res = await fetch("/api/combos");
      if (!res.ok) throw new Error(`Failed to fetch combos: ${res.status}`);
      const data = await res.json();
      setCombos(data.combos || []);
    } catch (error) {
      console.error("Error fetching combos:", error);
      setCombos([]);
    }
  };

  useEffect(() => {
    if (isOpen) fetchCombos();
  }, [isOpen]);

  useEffect(() => {
    if (!isOpen) return;
    (async () => {
      try {
        const res = await fetch("/api/mock/status", { cache: "no-store" });
        if (!res.ok) return;
        const data = await res.json();
        const map: Record<string, boolean> = {};
        for (const [name, entry] of Object.entries<any>(data?.providers || {})) {
          if (entry?.effective === "mock") map[name] = true;
        }
        setMockByAlias(map);
      } catch {
        /* badges stay hidden on error */
      }
    })();
  }, [isOpen]);

  const fetchProviderNodes = async () => {
    try {
      const res = await fetch("/api/provider-nodes");
      if (!res.ok) throw new Error(`Failed to fetch provider nodes: ${res.status}`);
      const data = await res.json();
      setProviderNodes(data.nodes || []);
    } catch (error) {
      console.error("Error fetching provider nodes:", error);
      setProviderNodes([]);
    }
  };

  useEffect(() => {
    if (isOpen) fetchProviderNodes();
  }, [isOpen]);

  const fetchCustomModels = async () => {
    try {
      const res = await fetch("/api/models/custom");
      if (!res.ok) throw new Error(`Failed to fetch custom models: ${res.status}`);
      const data = await res.json();
      setCustomModels(data.models || []);
    } catch (error) {
      console.error("Error fetching custom models:", error);
      setCustomModels([]);
    }
  };

  useEffect(() => {
    if (isOpen) fetchCustomModels();
  }, [isOpen]);

  useEffect(() => {
    if (!isOpen) return;
    (async () => {
      try {
        const res = await fetch("/api/models/alias", { cache: "no-store" });
        if (!res.ok) return;
        const data = await res.json();
        if (data && data.aliases && typeof data.aliases === "object") {
          setFetchedAliases(data.aliases as Record<string, string>);
        }
      } catch {
        /* keep the empty fallback */
      }
    })();
  }, [isOpen]);

  const fetchDisabledMap = async () => {
    try {
      const res = await fetch("/api/models/disabled", { cache: "no-store" });
      if (!res.ok) throw new Error(`Failed to fetch disabled: ${res.status}`);
      const data = await res.json();
      if (data.disabled && typeof data.disabled === "object") setDisabledMap(data.disabled);
      else if (Array.isArray(data.ids)) setDisabledMap({});
      else setDisabledMap({});
    } catch {
      setDisabledMap({});
    }
  };

  useEffect(() => {
    if (isOpen) fetchDisabledMap();
  }, [isOpen]);

  // Fetch live model lists (kilo free-models, opencode-zen / openrouter /
  // opencode fetchers) + per-provider freeOnly filters so the modal's groups
  // match the provider page's Available Models exactly.
  useEffect(() => {
    if (!isOpen) return;

    const liveCapable = Object.entries(AI_PROVIDERS)
      .filter(([id, p]) => id === "kilocode" || !!p.modelsFetcher)
      .map(([id]) => id);

    Promise.all(
      liveCapable.map(async (id) => {
        const alias = getProviderAlias(id);
        const models = await fetchLiveModels(id, alias);
        return [alias, models] as const;
      })
    )
      .then((entries) => {
        const map: Record<string, LiveModel[]> = {};
        for (const [alias, models] of entries) map[alias] = models;
        setLiveModelsByAlias(map);
      })
      .catch(() => {});

    // Read freeOnly through the same loader the provider page uses, so a failed
    // GET falls back to localStorage instead of silently dropping the filter and
    // desynchronising the two surfaces.
    const aliases = Object.keys(AI_PROVIDERS).map((id) => getProviderAlias(id));
    Promise.all(
      [...new Set(aliases)].map(async (alias) => [alias, await loadFreeOnly(alias)] as const)
    )
      .then((entries) => {
        const map: Record<string, boolean> = {};
        for (const [alias, freeOnly] of entries) map[alias] = freeOnly;
        setFreeOnlyByAlias(map);
      })
      .catch(() => {});
  }, [isOpen]);

  const allProviders = useMemo(() => ({ ...OAUTH_PROVIDERS, ...FREE_PROVIDERS, ...FREE_TIER_PROVIDERS, ...APIKEY_PROVIDERS }), []);

  // The prop wins when the caller supplies one; otherwise use what we fetched.
  // Pickers that omit the prop would otherwise see an empty map and, for
  // compatible providers, degrade to a single literal placeholder row.
  const resolvedAliases = useMemo(
    () => (Object.keys(modelAliases).length > 0 ? modelAliases : fetchedAliases),
    [modelAliases, fetchedAliases],
  );

  // Group models by provider with priority order
  const groupedModels = useMemo(() => {
    const groups: Record<string, ModelGroup> = {};

    const PROVIDER_AS_MODEL_KINDS = new Set(["webSearch", "webFetch"]);
    const TYPED_KINDS = new Set(["image", "tts", "stt", "embedding", "imageToText"]);
    const ALLOW_PROVIDER_FALLBACK_KINDS = new Set(["tts", "image", "webFetch"]);

    const filterByKind = (models: any[]) => {
      if (!kindFilter || !TYPED_KINDS.has(kindFilter)) return models;
      return models.filter((m) => m.isPlaceholder || m.type === kindFilter);
    };

    const isDisabled = (alias: string, modelId: string) => {
      const arr = disabledMap[alias];
      return Array.isArray(arr) && arr.includes(modelId);
    };

    const activeConnectionIds = filteredActiveProviders.map(p => p.provider);

    const noAuthIds = kindFilter
      ? NO_AUTH_PROVIDER_IDS.filter((id) => (AI_PROVIDERS[id]?.serviceKinds || ["llm"]).includes(kindFilter))
      : NO_AUTH_PROVIDER_IDS;

    const providerIdsToShow = new Set([
      ...activeConnectionIds,
      ...noAuthIds,
    ]);

    const sortedProviderIds = [...providerIdsToShow].sort((a, b) => {
      const indexA = PROVIDER_ORDER.indexOf(a);
      const indexB = PROVIDER_ORDER.indexOf(b);
      return (indexA === -1 ? 999 : indexA) - (indexB === -1 ? 999 : indexB);
    });

    sortedProviderIds.forEach((providerId) => {
      // hidden:true providers (devin-cli, mimo-free) reach the picker via
      // NO_AUTH_PROVIDER_IDS. Without this they render phantom groups the
      // Providers page never shows.
      if (AI_PROVIDERS[providerId]?.hidden) return;

      const alias = getProviderAlias(providerId);
      const providerInfo = allProviders[providerId] || { name: providerId, color: "#666" };
      const isCustomProvider = isOpenAICompatibleProvider(providerId) || isAnthropicCompatibleProvider(providerId);

      if (kindFilter && PROVIDER_AS_MODEL_KINDS.has(kindFilter)) {
        groups[providerId] = {
          name: providerInfo.name,
          alias,
          color: providerInfo.color,
          models: [{ id: providerId, name: providerInfo.name, value: providerId }],
        };
        return;
      }

      if (providerInfo.passthroughModels) {
        const aliasModels = Object.entries(resolvedAliases)
          .filter(([, fullModel]) => fullModel.startsWith(`${alias}/`))
          .map(([aliasName, fullModel]) => ({
            id: fullModel.replace(`${alias}/`, ""),
            name: aliasName,
            value: fullModel,
          }));

        let combined = aliasModels;
        if (kindFilter && TYPED_KINDS.has(kindFilter)) {
          combined = getModelsByProviderId(providerId)
            .filter((m) => m.type === kindFilter && !isDisabled(alias, m.id))
            .map((m) => ({ id: m.id, name: m.name, value: `${alias}/${m.id}`, type: m.type }));
          if (combined.length === 0 && ALLOW_PROVIDER_FALLBACK_KINDS.has(kindFilter)) {
            const supports = (providerInfo.serviceKinds || ["llm"]).includes(kindFilter);
            if (supports) combined = [{ id: providerId, name: providerInfo.name, value: alias }];
          }
        } else if (!kindFilter) {
          const built = buildAvailableModels({
            catalogModels: getModelsByProviderId(providerId) as any,
            liveModels: liveModelsByAlias[alias] || [],
            customModels: customModels as any,
            modelAliases: resolvedAliases,
            disabledIds: disabledMap[alias] || [],
            providerAlias: alias,
            type: "llm",
            freeOnly: freeOnlyByAlias[alias] || false,
          });
          const mapped = built.enabledRows.map((r) => ({
            id: r.id,
            name: r.name,
            value: r.fullModel,
            type: r.type,
            isFree: r.isFree,
            isCustom: r.source === "custom" || r.source === "legacyAlias",
          }));
          if (mapped.length > 0) combined = mapped;
        }

        if (combined.length > 0) {
          const matchedNode = providerNodes.find(node => node.id === providerId);
          const displayName = matchedNode?.name || providerInfo.name;
          groups[providerId] = { name: displayName, alias, color: providerInfo.color, models: combined };
        } else if (combined.length === 0 && kindFilter === null && (providerInfo.serviceKinds || ["llm"]).includes("llm")) {
          const matchedNode = providerNodes.find(node => node.id === providerId);
          groups[providerId] = {
            name: matchedNode?.name || providerInfo.name,
            alias,
            color: providerInfo.color,
            models: [{ id: providerId, name: matchedNode?.name || providerInfo.name, value: alias }],
          };
        }
      } else if (isCustomProvider) {
        if (kindFilter && TYPED_KINDS.has(kindFilter)) return;
        const connection = activeProviders.find(p => p.provider === providerId);
        const matchedNode = providerNodes.find(node => node.id === providerId);
        const displayName = connection?.name || matchedNode?.name || providerInfo.name;
        const nodePrefix = connection?.providerSpecificData?.prefix || matchedNode?.prefix || providerId;
        const nodeModels = Object.entries(resolvedAliases)
          .filter(([, fullModel]) => fullModel.startsWith(`${providerId}/`))
          .map(([aliasName, fullModel]) => ({
            id: fullModel.replace(`${providerId}/`, ""),
            name: aliasName,
            value: `${nodePrefix}/${fullModel.replace(`${providerId}/`, "")}`,
          }));
        const modelsToShow = nodeModels.length > 0 ? nodeModels : [{
          id: `__placeholder__${providerId}`,
          name: `${nodePrefix}/model-id`,
          value: `${nodePrefix}/model-id`,
          isPlaceholder: true,
        }];
        groups[providerId] = { name: displayName, alias: nodePrefix, color: providerInfo.color, models: modelsToShow, isCustom: true, hasModels: nodeModels.length > 0 };
      } else {
        if (kindFilter && TYPED_KINDS.has(kindFilter)) {
          const allCatalog = getModelsByProviderId(providerId);
          const hardcodedModels = allCatalog.filter((m) => !isDisabled(alias, m.id));
          const customRows = getProviderCustomModelRows({
            customModels: customModels as any,
            modelAliases: resolvedAliases,
            providerAlias: alias,
            builtInModels: allCatalog as any,
            type: kindFilter as any,
          });
          const customAliasModels = customRows
            .filter((r) => r.source === "legacyAlias")
            .map((r) => ({ id: r.id, name: r.alias || r.id, value: r.fullModel, type: r.type, isCustom: true }));
          const customRegisteredModels = customRows
            .filter((r) => r.source === "custom")
            .map((r) => ({ id: r.id, name: r.name || r.id, value: r.fullModel, type: r.type, isCustom: true }));

          let allModels = filterByKind([
            ...hardcodedModels.map((m) => ({ id: m.id, name: m.name, value: `${alias}/${m.id}`, type: m.type })),
            ...customAliasModels,
            ...customRegisteredModels,
          ]);

          if (allModels.length === 0 && ALLOW_PROVIDER_FALLBACK_KINDS.has(kindFilter)) {
            const supports = (providerInfo.serviceKinds || ["llm"]).includes(kindFilter);
            if (supports) allModels = [{ id: providerId, name: providerInfo.name, value: alias }];
          }

          if (allModels.length > 0) {
            groups[providerId] = { name: providerInfo.name, alias, color: providerInfo.color, models: allModels };
          } else if (allModels.length === 0 && kindFilter === null && (providerInfo.serviceKinds || ["llm"]).includes("llm")) {
            groups[providerId] = { name: providerInfo.name, alias, color: providerInfo.color, models: [{ id: providerId, name: providerInfo.name, value: alias }] };
          }
        } else {
          const built = buildAvailableModels({
            catalogModels: getModelsByProviderId(providerId) as any,
            liveModels: liveModelsByAlias[alias] || [],
            customModels: customModels as any,
            modelAliases: resolvedAliases,
            disabledIds: disabledMap[alias] || [],
            providerAlias: alias,
            type: "llm",
            freeOnly: freeOnlyByAlias[alias] || false,
          });
          let allModels = built.enabledRows.map((r) => ({
            id: r.id,
            name: r.name,
            value: r.fullModel,
            type: r.type,
            isFree: r.isFree,
            isCustom: r.source === "custom" || r.source === "legacyAlias",
          }));

          // Gate on the provider being genuinely empty, not on the enabled set
          // being empty. Otherwise a fully-disabled provider still offers a bare
          // `{ value: alias }` row, which resolves to the wrong provider
          // (core/model/mod.rs falls back to its default route).
          if (built.allRows.length === 0 && kindFilter === null && (providerInfo.serviceKinds || ["llm"]).includes("llm")) {
            allModels = [{ id: providerId, name: providerInfo.name, value: alias }];
          }

          if (allModels.length > 0) {
            groups[providerId] = { name: providerInfo.name, alias, color: providerInfo.color, models: allModels };
          }
        }
      }
    });

    return groups;
  }, [filteredActiveProviders, resolvedAliases, allProviders, providerNodes, customModels, kindFilter, disabledMap, liveModelsByAlias, freeOnlyByAlias, catalogReady]);

  // Filter combos by search query (and hide combos when kindFilter is set — combos are LLM-only by design)
  const filteredCombos = useMemo(() => {
    if (kindFilter) return [];
    if (!searchQuery.trim()) return combos;
    const query = searchQuery.toLowerCase();
    return combos.filter(c => c.name.toLowerCase().includes(query));
  }, [combos, searchQuery, kindFilter]);

  // Filter models by search query
  // Sort models alphabetically, with added models floated to top
  const sortModels = (models: Model[]) => {
    const added = models.filter((m) => addedModelValues.includes(m.value)).sort((a, b) => a.name.localeCompare(b.name));
    const rest = models.filter((m) => !addedModelValues.includes(m.value)).sort((a, b) => a.name.localeCompare(b.name));
    return [...added, ...rest];
  };

  const filteredGroups = useMemo(() => {
    const query = searchQuery.trim().toLowerCase();

    const filtered: Record<string, ModelGroup> = {};

    Object.entries(groupedModels).forEach(([providerId, group]) => {
      let models = group.models;
      // Filter by input-modality capability (vision/pdf/audioInput/videoInput).
      if (capFilter) {
        models = models.filter((m) => getCaps(m.value)?.[capFilter] === true);
        if (models.length === 0) return;
      }
      const matchedModels = query
        ? models.filter(
            (m) =>
              m.name.toLowerCase().includes(query) ||
              m.id.toLowerCase().includes(query) ||
              m.value.toLowerCase().includes(query),
          )
        : sortModels(models);

      // A provider-name hit should show that provider's models, not an empty
      // group. Matching on the alias matters too: 39 of 105 providers have an
      // alias that is not a substring of the display name ("kilo" -> Kilo Code).
      const groupMatches = group.name.toLowerCase().includes(query) ||
        group.alias.toLowerCase().includes(query);
      const visibleModels = groupMatches ? sortModels(models) : matchedModels;

      // Never emit a group with zero models — it renders a bare "(0)" header
      // and suppresses the "No models found" fallback.
      if (visibleModels.length > 0) {
        filtered[providerId] = {
          ...group,
          models: visibleModels,
        };
      }
    });

    return filtered;
  }, [groupedModels, searchQuery, addedModelValues, capFilter]);

  const handleSelect = (model: Model) => {
    onSelect(model);
    if (closeOnSelect) {
      onClose();
      setSearchQuery("");
    }
  };

  const handleApplyMulti = () => {
    if (onSelectIds) onSelectIds(selectedIds);
    onClose();
    setSearchQuery("");
    setSelectedIds([]);
  };

  return (
    <Modal
      isOpen={isOpen}
      onClose={() => {
        onClose();
        setSearchQuery("");
      }}
      title={title}
      size="md"
      className="p-4!"
      footer={
        selectionMode === "multi" ? (
          <div className="flex w-full items-center justify-between gap-2">
            <span className="text-xs text-text-muted">
              {selectedIds.length} selected
            </span>
            <div className="flex items-center gap-2">
              <Button
                variant="ghost"
                size="sm"
                onClick={() => {
                  onClose();
                  setSearchQuery("");
                }}
              >
                Close
              </Button>
              <Button
                size="sm"
                onClick={handleApplyMulti}
                disabled={selectedIds.length === 0}
              >
                Apply {selectedIds.length}
              </Button>
            </div>
          </div>
        ) : !closeOnSelect ? (
          <Button
            onClick={() => {
              onClose();
              setSearchQuery("");
            }}
            fullWidth
          >
            Done
          </Button>
        ) : null
      }
    >
      {/* Search - compact */}
      <div className="mb-3">
        <div className="relative">
          <span className="material-symbols-outlined absolute left-2.5 top-1/2 -translate-y-1/2 text-text-muted text-[16px]">
            search
          </span>
          <input
            type="text"
            placeholder="Search..."
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            className="w-full pl-8 pr-3 py-1.5 bg-surface border border-border rounded text-xs focus:outline-none focus:ring-1 focus:ring-primary/50"
          />
        </div>
      </div>

      {/* Provider outline - quick jump when many providers */}
      {Object.keys(filteredGroups).length > 1 && (
        <div className="flex gap-1.5 overflow-x-auto pb-2 -mx-1 px-1 scrollbar-thin">
          {Object.entries(filteredGroups).map(([pid, grp]) => (
            <button
              key={`outline-${pid}`}
              onClick={() => {
                const el = document.getElementById(`provider-section-${pid}`);
                const container = listRef.current;
                if (el && container) container.scrollTo({ top: el.offsetTop - container.offsetTop - 8, behavior: "smooth" });
                else el?.scrollIntoView({ behavior: "smooth", block: "start" });
              }}
              className="shrink-0 inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full border text-[11px] font-medium bg-surface border-border hover:border-primary/50 hover:bg-primary/5 transition-colors"
              title={`Jump to ${grp.name}`}
            >
              <span className="w-2 h-2 rounded-full shrink-0" style={{ backgroundColor: grp.color }} />
              {grp.alias}
              <span className="opacity-60">({grp.models.length})</span>
            </button>
          ))}
        </div>
      )}

      {/* Models grouped by provider - compact */}
      <div ref={listRef} className="max-h-[400px] overflow-y-auto space-y-3 scroll-smooth">
        {/* Combos section - always first */}
        {filteredCombos.length > 0 && (
          <div>
            <div className="flex items-center gap-1.5 mb-1.5 sticky top-0 bg-surface py-0.5">
              <span className="material-symbols-outlined text-primary text-[14px]">layers</span>
              <span className="text-xs font-medium text-primary">Combos</span>
              <span className="text-[10px] text-text-muted">({filteredCombos.length})</span>
            </div>
            <div className="flex flex-wrap gap-1.5">
              {filteredCombos.map((combo) => {
                const isSelected = Array.isArray(selectedModel)
                  ? selectedModel.includes(combo.name)
                  : selectedModel === combo.name;
                return (
                  <button
                    key={combo.id}
                    onClick={() => handleSelect({ id: combo.name, name: combo.name, value: combo.name })}
                    className={`
                      px-2 py-1 rounded-xl text-xs font-medium transition-all border hover:cursor-pointer
                      ${isSelected
                        ? "bg-primary text-white border-primary"
                        : "bg-surface border-border text-text-main hover:border-primary/50 hover:bg-primary/5"
                      }
                    `}
                  >
                    {combo.name}
                  </button>
                );
              })}
            </div>
          </div>
        )}

        {/* Provider models */}
        {Object.entries(filteredGroups).map(([providerId, group]) => (
          <div key={providerId} id={`provider-section-${providerId}`}>
            {/* Provider header */}
            <div className="flex items-center gap-1.5 mb-1.5 sticky top-0 bg-surface py-0.5">
              <div
                className="w-2 h-2 rounded-full"
                style={{ backgroundColor: group.color }}
              />
              <span className="text-xs font-medium text-primary">
                {group.name}
              </span>
              <span className="text-[10px] text-text-muted">
                ({group.models.length})
              </span>
              {mockByAlias[providerId] && (
                <span
                  className="text-[10px] font-medium text-purple-600 dark:text-purple-400"
                  title="This provider is in Mock mode (simulated locally, no API key used)"
                >
                  🧪 mock
                </span>
              )}
            </div>

            <div
              aria-label="Available models"
              className="flex flex-wrap gap-1.5">
              {group.models.map((model) => {
                const isPlaceholder = model.isPlaceholder;
                const isLlm = model.type === "llm";
                const favAlias = getProviderAlias(providerId);
                const fav = isLlm ? isFavorite(favAlias, model.id) : false;
                const isMulti = selectionMode === "multi";
                const isMultiSelected = isMulti && selectedIds.includes(model.value);
                const isSingleSelected = !isMulti && (Array.isArray(selectedModel)
                  ? selectedModel.includes(model.value)
                  : selectedModel === model.value);
                const rowClick = () => {
                  if (isMulti) toggleSelect(model.value);
                  else handleSelect(model);
                };
                return (
                  // A native button is the whole affordance: Enter and Space
                  // come free, it announces as a button, and it is one tab stop
                  // per model. The star and the multi-select checkbox are
                  // siblings — interactive content may not nest inside a button.
                  <div className="inline-flex items-center gap-0.5" key={model.value}>
                    {isLlm && (
                      <button
                        type="button"
                        onClick={() => toggleFavorite(favAlias, model.id)}
                        className="shrink-0 rounded p-0.5 hover:bg-black/5 dark:hover:bg-white/5"
                        title={fav ? "Remove from favorites" : "Add to favorites"}
                        aria-label={fav ? "Remove from favorites" : "Add to favorites"}
                      >
                        <span className={`material-symbols-outlined text-[14px] ${fav ? "text-yellow-400" : "text-text-muted"}`}>
                          {fav ? "star" : "star_outline"}
                        </span>
                      </button>
                    )}
                    {isMulti && (
                      <input
                        type="checkbox"
                        checked={isMultiSelected}
                        onChange={() => toggleSelect(model.value)}
                        className="h-3.5 w-3.5 rounded border-gray-300 text-primary focus:ring-primary shrink-0"
                        aria-label={`Select ${model.name}`}
                      />
                    )}
                    <button
                      type="button"
                      onClick={rowClick}
                      aria-pressed={isMultiSelected || isSingleSelected}
                      title={isPlaceholder ? "Select to pre-fill, then edit model ID in the input" : undefined}
                      className={`
                        inline-flex items-center gap-1 px-2 py-1 rounded-xl text-xs font-medium transition-all border hover:cursor-pointer
                        ${isPlaceholder
                          ? "border-dashed border-border text-text-muted hover:border-primary/50 hover:text-primary bg-surface italic"
                          : isMultiSelected
                            ? "border-primary bg-primary/10 text-text-main"
                            : isSingleSelected
                              ? "bg-primary text-white border-primary"
                              : "bg-surface border-border text-text-main hover:border-primary/50 hover:bg-primary/5"
                        }
                      `}
                    >
                    {isPlaceholder ? (
                      <span className="flex items-center gap-1">
                        <span className="material-symbols-outlined text-[11px]">edit</span>
                        {model.name}
                      </span>
                    ) : model.isCustom ? (
                      <span className="flex items-center gap-1">
                        {model.name}
                        <span className="text-[9px] opacity-60 font-normal">custom</span>
                        <CapacityBadges caps={getCaps(model.value)} size={12} colorOverride={isMultiSelected || isSingleSelected ? "text-white/80" : undefined} />
                      </span>
                    ) : (
                      <span className="flex items-center gap-1">
                        {model.name}
                        <CapacityBadges caps={getCaps(model.value)} size={12} colorOverride={isMultiSelected || isSingleSelected ? "text-white/80" : undefined} />
                      </span>
                    )}
                    </button>
                  </div>
                );
              })}
            </div>
          </div>
        ))}

        {Object.keys(filteredGroups).length === 0 && filteredCombos.length === 0 && (
          <div className="text-center py-4 text-text-muted">
            <span className="material-symbols-outlined text-2xl mb-1 block">
              search_off
            </span>
            <p className="text-xs">No models found</p>
          </div>
        )}
      </div>
    </Modal>
  );
}
