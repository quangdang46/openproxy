import { create } from "zustand";

// Catalog returned by GET /api/catalog (mirrors src/core/model/provider_catalog.json).
interface CatalogModel {
  id: string;
  name?: string;
  kind: string;
}

interface CatalogModelsEntry {
  alias: string;
  models: CatalogModel[];
}

interface CatalogResponse {
  providerIdToAlias?: Record<string, string>;
  providerModels?: CatalogModelsEntry[];
}

interface CatalogState {
  loaded: boolean;
  loading: boolean;
  modelsByAlias: Record<string, CatalogModel[]>;
  providerIdToAlias: Record<string, string>;
  load: () => Promise<void>;
}

export const useCatalogStore = create<CatalogState>((set, get) => ({
  loaded: false,
  loading: false,
  modelsByAlias: {},
  providerIdToAlias: {},
  load: async () => {
    const state = get();
    if (state.loaded || state.loading) return;
    set({ loading: true });
    try {
      const res = await fetch("/api/catalog", { cache: "no-store" });
      if (!res.ok) {
        set({ loading: false });
        return;
      }
      const data: CatalogResponse = await res.json();
      const modelsByAlias: Record<string, CatalogModel[]> = {};
      for (const entry of data.providerModels || []) {
        if (entry?.alias) modelsByAlias[entry.alias] = entry.models || [];
      }
      set({
        loaded: true,
        loading: false,
        modelsByAlias,
        providerIdToAlias: data.providerIdToAlias || {},
      });
    } catch {
      set({ loading: false });
    }
  },
}));

// Kick off the fetch on module import in the browser.
if (typeof window !== "undefined") {
  useCatalogStore.getState().load();
}

/**
 * React hook that ensures the catalog is loaded and re-renders the caller
 * when it arrives. Returns true once data is available.
 */
export function useEnsureCatalog(): boolean {
  const loaded = useCatalogStore((s) => s.loaded);
  const loading = useCatalogStore((s) => s.loading);
  if (!loaded && !loading && typeof window !== "undefined") {
    useCatalogStore.getState().load();
  }
  return loaded;
}
