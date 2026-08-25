// Single source of truth for "Available Models" on a provider.
//
// Both the provider detail page (ProviderDetailPageClient) and the model
// picker (ModelSelectModal) must show an IDENTICAL list. They do so by
// consuming `buildAvailableModels` (pure) and `useAvailableModels` (hook).
//
// The merged list is: catalog models + live-fetched models (kilo free-models,
// opencode-zen / openrouter / opencode fetchers) + user custom/legacy-alias
// rows, deduplicated by id, llm-filtered, with a consistent disabled flag and
// id normalization.

import { useState, useEffect, useCallback, useMemo } from "react";
// Catalog loads async into the store; subscribe so rows recompute on arrival.
import { getModelsByProviderId, useEnsureCatalog } from "@/shared/constants/models";
import { useCatalogStore } from "@/store/catalogStore";
import { getProviderAlias, AI_PROVIDERS } from "@/shared/constants/providers";
import {
  getProviderCustomModelRows,
  type CustomModelEntry,
} from "@/shared/utils/providerCustomModels";
import { fetchSuggestedModels } from "@/shared/utils/providerModelsFetcher";

export type AvailableModelSource = "catalog" | "live" | "custom" | "legacyAlias";

export interface AvailableModelRow {
  id: string;
  name: string;
  fullModel: string;
  source: AvailableModelSource;
  type: string;
  isFree: boolean;
  disabled: boolean;
  alias?: string;
}

export interface LiveModel {
  id: string;
  name?: string;
  isFree?: boolean;
}

export interface CatalogModelInput {
  id: string;
  name?: string;
  type?: string;
  kind?: string;
  isFree?: boolean;
  [key: string]: unknown;
}

export interface BuildAvailableModelsInput {
  catalogModels: ReadonlyArray<CatalogModelInput>;
  liveModels?: ReadonlyArray<LiveModel>;
  customModels?: ReadonlyArray<CustomModelEntry>;
  modelAliases?: Record<string, string>;
  disabledIds?: ReadonlyArray<string>;
  providerAlias: string;
  type?: string;
  freeOnly?: boolean;
}

export interface BuildAvailableModelsResult {
  /** Custom + legacyAlias rows. Always shown (not subject to freeOnly). */
  customRows: AvailableModelRow[];
  /** Catalog + live rows that are enabled and pass the freeOnly filter. */
  enabledCoreRows: AvailableModelRow[];
  /** Catalog + live rows that are disabled. */
  disabledCoreRows: AvailableModelRow[];
  /** customRows + enabledCoreRows — the visible enabled set. */
  enabledRows: AvailableModelRow[];
  /** Every merged row (custom + enabled core + disabled core). */
  allRows: AvailableModelRow[];
  /** Every catalog + live id (custom excluded) — full merged core list. */
  allCoreIds: string[];
}

const FREE_ID_RE = /(:free|-free)$/i;

export function isFreeModelId(
  id: string,
  _providerAlias: string,
  explicitFree?: boolean
): boolean {
  if (explicitFree) return true;
  return FREE_ID_RE.test(id);
}

function modelKind(model: CatalogModelInput): string | undefined {
  const kind = model.kind;
  if (typeof kind === "string" && kind) return kind;
  const type = model.type;
  if (typeof type === "string" && type) return type;
  return undefined;
}

/**
 * Pure builder. Merges catalog + live + custom models into a single,
 * deduplicated, llm-filtered list with consistent id normalization.
 *
 * - Custom/legacyAlias rows are always returned (never freeOnly-filtered).
 * - Catalog/live rows respect `disabledIds` and the `freeOnly` filter.
 */
export function buildAvailableModels(
  input: BuildAvailableModelsInput
): BuildAvailableModelsResult {
  const {
    catalogModels = [],
    liveModels = [],
    customModels = [],
    modelAliases = {},
    disabledIds = [],
    providerAlias,
    type = "llm",
    freeOnly = false,
  } = input;

  const disabledSet = new Set<string>(disabledIds);

  // 1. Catalog + live merged, deduplicated by id.
  const coreById = new Map<string, AvailableModelRow>();
  const pushCore = (
    id: string,
    name: string | undefined,
    source: "catalog" | "live",
    explicitFree?: boolean
  ) => {
    if (!id || coreById.has(id)) return;
    coreById.set(id, {
      id,
      name: name || id,
      fullModel: `${providerAlias}/${id}`,
      source,
      type,
      isFree: isFreeModelId(id, providerAlias, explicitFree),
      disabled: disabledSet.has(id),
    });
  };

  for (const m of catalogModels) {
    const kind = modelKind(m);
    if (kind && kind !== type) continue;
    const isFree =
      typeof m.isFree === "boolean" ? m.isFree : undefined;
    pushCore(m.id, m.name, "catalog", isFree);
  }
  for (const m of liveModels) {
    pushCore(m.id, m.name, "live", m.isFree);
  }

  // 2. Custom + legacyAlias rows (always shown).
  const customRowsRaw = getProviderCustomModelRows({
    customModels: customModels as CustomModelEntry[],
    modelAliases,
    providerAlias,
    builtInModels: catalogModels as Array<{ id: string }>,
    type,
  });
  const customRows: AvailableModelRow[] = customRowsRaw.map((r) => ({
    id: r.id,
    name: r.name || r.id,
    fullModel: r.fullModel,
    source: r.source,
    type: r.type,
    isFree: false,
    disabled: disabledSet.has(r.id),
    alias: r.alias,
  }));

  // 3. Split core rows by disabled + freeOnly.
  const coreRows = Array.from(coreById.values());
  const enabledCoreRows = coreRows.filter(
    (r) => !r.disabled && (!freeOnly || r.isFree)
  );
  const disabledCoreRows = coreRows.filter((r) => r.disabled);

  const enabledRows = [...customRows, ...enabledCoreRows];
  const allRows = [...customRows, ...coreRows];
  const allCoreIds = coreRows.map((r) => r.id);

  return {
    customRows,
    enabledCoreRows,
    disabledCoreRows,
    enabledRows,
    allRows,
    allCoreIds,
  };
}

/**
 * Fetch the live model list for a provider. Resilient: on any failure it
 * returns [] so the caller degrades to catalog + custom rows.
 *
 * - kilocode → dedicated /api/providers/kilo/free-models endpoint (free by def).
 * - providers with a `modelsFetcher` (opencode-zen, openrouter, opencode) →
 *   the existing suggested-models proxy path.
 */
export async function fetchLiveModels(
  providerId: string,
  providerAlias: string
): Promise<LiveModel[]> {
  if (providerId === "kilocode" || providerAlias === "kc") {
    try {
      const res = await fetch("/api/providers/kilo/free-models");
      if (res.ok) {
        const data = await res.json();
        const models = Array.isArray(data?.models) ? data.models : [];
        return models.map((m: { id?: string; name?: string }) => ({
          id: m.id ?? "",
          name: m.name,
          isFree: true,
        })).filter((m: LiveModel) => !!m.id);
      }
    } catch {
      return [];
    }
    return [];
  }

  const provider = AI_PROVIDERS[providerId];
  const fetcher = provider?.modelsFetcher;
  if (fetcher?.url && fetcher?.type) {
    try {
      const suggested = await fetchSuggestedModels(fetcher);
      return suggested
        .map((m) => ({
          id: m.id,
          name: m.name,
          isFree: /(:free|-free)$/i.test(m.id),
        }))
        .filter((m) => !!m.id);
    } catch {
      return [];
    }
  }

  return [];
}

// ── Free-only filter persistence ────────────────────────────────────────
// Backend contract (may not exist yet — parallel work):
//   GET  /api/providers/filters → { filters: { [alias]: { freeOnly: bool } } }
//   PUT  /api/providers/filters  → { alias, freeOnly }
// Falls back to localStorage when the endpoint is unavailable.

const FREE_ONLY_LS_PREFIX = "openproxy:freeOnly:";

async function loadFreeOnly(alias: string): Promise<boolean> {
  try {
    const res = await fetch("/api/providers/filters", { cache: "no-store" });
    if (res.ok) {
      const data = await res.json();
      const filters = (data && data.filters) || {};
      const entry = filters[alias];
      if (entry && typeof entry.freeOnly === "boolean") {
        return entry.freeOnly;
      }
    }
  } catch {
    // ignore — fall through to localStorage
  }
  try {
    const v = localStorage.getItem(FREE_ONLY_LS_PREFIX + alias);
    if (v !== null) return v === "1";
  } catch {
    // ignore
  }
  return false;
}

async function saveFreeOnly(alias: string, value: boolean): Promise<void> {
  try {
    localStorage.setItem(FREE_ONLY_LS_PREFIX + alias, value ? "1" : "0");
  } catch {
    // ignore
  }
  try {
    await fetch("/api/providers/filters", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ alias, freeOnly: value }),
    });
  } catch {
    // ignore — localStorage fallback already applied
  }
}

// ── Favorites (star) persistence ────────────────────────────────────────
// Backend contract:
//   GET  /api/models/favorites → { favorites: { [alias]: string[] } }
//   PUT  /api/models/favorites  → { alias, favorites: string[] }
// Module-level singleton so the multi-provider ModelSelectModal and the
// single-provider ProviderDetailPageClient share ONE cache and update
// instantly. Both key favorites by getProviderAlias(providerId).

let _favorites: Record<string, string[]> = {};
const _favListeners = new Set<() => void>();
let _favLoaded = false;

function _emitFav(): void {
  _favListeners.forEach((l) => l());
}

export async function loadFavorites(): Promise<void> {
  try {
    const res = await fetch("/api/models/favorites", { cache: "no-store" });
    if (res.ok) {
      const data = await res.json();
      if (data && data.favorites && typeof data.favorites === "object") {
        _favorites = data.favorites as Record<string, string[]>;
      }
    }
  } catch {
    // ignore — start empty
  } finally {
    _favLoaded = true;
    _emitFav();
  }
}

export function isFavoriteModel(alias: string, id: string): boolean {
  return Array.isArray(_favorites[alias]) && _favorites[alias].includes(id);
}

export async function toggleFavoriteModel(alias: string, id: string): Promise<void> {
  const current = _favorites[alias] || [];
  const next = current.includes(id)
    ? current.filter((x) => x !== id)
    : [...current, id];
  // Optimistic update so the star flips instantly in every view.
  _favorites = { ..._favorites, [alias]: next };
  _emitFav();
  try {
    await fetch("/api/models/favorites", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ alias, favorites: next }),
    });
  } catch {
    // Revert on failure.
    _favorites = { ..._favorites, [alias]: current };
    _emitFav();
  }
}

/**
 * Shared favorites store. Any component that calls this re-renders when the
 * favorites cache changes, so the modal and provider page stay in sync.
 */
export function useFavorites(): {
  favorites: Record<string, string[]>;
  isFavorite: (alias: string, id: string) => boolean;
  toggleFavorite: (alias: string, id: string) => void;
} {
  const [, force] = useState(0);
  useEffect(() => {
    const l = () => force((x) => x + 1);
    _favListeners.add(l);
    if (!_favLoaded) void loadFavorites();
    return () => {
      _favListeners.delete(l);
    };
  }, []);
  return {
    favorites: _favorites,
    isFavorite: isFavoriteModel,
    toggleFavorite: (alias: string, id: string) => {
      void toggleFavoriteModel(alias, id);
    },
  };
}

export interface UseAvailableModelsResult extends BuildAvailableModelsResult {
  liveModels: LiveModel[];
  customModels: CustomModelEntry[];
  modelAliases: Record<string, string>;
  disabledIds: string[];
  freeOnly: boolean;
  setFreeOnly: (value: boolean) => void;
  disable: (ids: string[]) => Promise<void>;
  enable: (id: string) => Promise<void>;
  disableAll: () => Promise<void>;
  enableAll: () => Promise<void>;
  loading: boolean;
  refresh: () => void;
  providerAlias: string;
}

/**
 * Hook returning the authoritative Available Models list for a provider,
 * plus mutation wrappers and the persisted freeOnly flag.
 */
export function useAvailableModels(providerId: string): UseAvailableModelsResult {
  const providerAlias = getProviderAlias(providerId);
  const catalogReady = useEnsureCatalog();
  useCatalogStore((s) => s.modelsByAlias);
  const catalogModels = useMemo(
    () => getModelsByProviderId(providerId) as unknown as CatalogModelInput[],
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [providerId, catalogReady]
  );
  const [liveModels, setLiveModels] = useState<LiveModel[]>([]);
  const [customModels, setCustomModels] = useState<CustomModelEntry[]>([]);
  const [modelAliases, setModelAliases] = useState<Record<string, string>>({});
  const [disabledIds, setDisabledIds] = useState<string[]>([]);
  const [freeOnly, setFreeOnlyState] = useState<boolean>(false);
  const [loading, setLoading] = useState<boolean>(true);

  const fetchLive = useCallback(async () => {
    try {
      const models = await fetchLiveModels(providerId, providerAlias);
      setLiveModels(models);
    } catch {
      setLiveModels([]);
    }
  }, [providerId, providerAlias]);

  const fetchCustom = useCallback(async () => {
    try {
      const res = await fetch("/api/models/custom", { cache: "no-store" });
      const data = await res.json();
      if (res.ok) setCustomModels(data.models || []);
    } catch {
      // ignore
    }
  }, []);

  const fetchAliases = useCallback(async () => {
    try {
      const res = await fetch("/api/models/alias");
      const data = await res.json();
      if (res.ok) setModelAliases(data.aliases || {});
    } catch {
      // ignore
    }
  }, []);

  const fetchDisabled = useCallback(async () => {
    try {
      const res = await fetch(
        `/api/models/disabled?providerAlias=${encodeURIComponent(providerAlias)}`,
        { cache: "no-store" }
      );
      const data = await res.json();
      if (res.ok) setDisabledIds(data.ids || []);
    } catch {
      // ignore
    }
  }, [providerAlias]);

  const refresh = useCallback(() => {
    fetchLive();
    fetchCustom();
    fetchAliases();
    fetchDisabled();
  }, [fetchLive, fetchCustom, fetchAliases, fetchDisabled]);

  useEffect(() => {
    setLoading(true);
    refresh();
    loadFreeOnly(providerAlias)
      .then(setFreeOnlyState)
      .finally(() => setLoading(false));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [providerAlias]);

  const result = useMemo(
    () =>
      buildAvailableModels({
        catalogModels,
        liveModels,
        customModels,
        modelAliases,
        disabledIds,
        providerAlias,
        type: "llm",
        freeOnly,
      }),
    [catalogModels, liveModels, customModels, modelAliases, disabledIds, providerAlias, freeOnly]
  );

  const disable = useCallback(
    async (ids: string[]) => {
      if (!ids.length) return;
      try {
        await fetch("/api/models/disabled", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ providerAlias, ids }),
        });
        await fetchDisabled();
      } catch {
        // ignore
      }
    },
    [providerAlias, fetchDisabled]
  );

  const enable = useCallback(
    async (id: string) => {
      try {
        await fetch(
          `/api/models/disabled?providerAlias=${encodeURIComponent(
            providerAlias
          )}&id=${encodeURIComponent(id)}`,
          { method: "DELETE" }
        );
        await fetchDisabled();
      } catch {
        // ignore
      }
    },
    [providerAlias, fetchDisabled]
  );

  const disableAll = useCallback(async () => {
    await disable(result.enabledRows.map((r) => r.id));
  }, [disable, result.enabledRows]);

  const enableAll = useCallback(async () => {
    try {
      await fetch(
        `/api/models/disabled?providerAlias=${encodeURIComponent(providerAlias)}`,
        { method: "DELETE" }
      );
      await fetchDisabled();
    } catch {
      // ignore
    }
  }, [providerAlias, fetchDisabled]);

  const setFreeOnly = useCallback(
    (value: boolean) => {
      setFreeOnlyState(value);
      saveFreeOnly(providerAlias, value);
    },
    [providerAlias]
  );

  // Favorites (star) — backed by the shared module-level store so the modal
  // and this provider page reflect the same persisted state instantly.
  const { isFavorite: isFav, toggleFavorite: toggleFav } = useFavorites();
  const isFavorite = useCallback(
    (id: string) => isFav(providerAlias, id),
    [providerAlias, isFav]
  );
  const toggleFavorite = useCallback(
    (id: string) => {
      toggleFav(providerAlias, id);
    },
    [providerAlias, toggleFav]
  );
  const loadFav = useCallback(() => {
    void loadFavorites();
  }, []);

  return {
    ...result,
    liveModels,
    customModels,
    modelAliases,
    disabledIds,
    freeOnly,
    setFreeOnly,
    disable,
    enable,
    disableAll,
    enableAll,
    loading,
    refresh,
    providerAlias,
    favorites: _favorites[providerAlias] || [],
    isFavorite,
    toggleFavorite,
    loadFavorites: loadFav,
  };
}
