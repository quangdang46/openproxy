import { create } from "zustand";

// Client-side settings cache with TTL (60s), mirroring the catalogStore
// pattern. Ported from 9router `src/store/settingsStore.js`.
//
// Goal: ~15 scattered `fetch("/api/settings")` calls collapse to ~1
// request per tab while the cache is fresh. Components that need a fresh
// copy (after PATCH, on explicit refresh) pass `{ force: true }`.

export const CLIENT_STORE_TTL_MS = 60000;

interface SettingsState {
  settings: Record<string, any> | null;
  loading: boolean;
  error: string | null;
  lastFetched: number;
  invalidate: () => void;
  fetchSettings: (opts?: { force?: boolean }) => Promise<Record<string, any> | null>;
  patchSettings: (patch: Record<string, any>) => Promise<Record<string, any> | null>;
}

export const useSettingsStore = create<SettingsState>((set, get) => ({
  settings: null,
  loading: false,
  error: null,
  lastFetched: 0,

  invalidate: () => set({ lastFetched: 0 }),

  // Skips network when cache is fresh; pass {force:true} to override
  fetchSettings: async ({ force = false } = {}) => {
    const { lastFetched, settings } = get();
    if (!force && settings && Date.now() - lastFetched < CLIENT_STORE_TTL_MS)
      return settings;
    set({ loading: true, error: null });
    try {
      const res = await fetch("/api/settings");
      const data = await res.json();
      if (res.ok) {
        set({ settings: data, loading: false, lastFetched: Date.now() });
        return data;
      }
      set({ error: data.error, loading: false });
    } catch (e) {
      set({ error: "Failed to fetch settings", loading: false });
    }
    return null;
  },

  // PATCH server + merge into local cache (no extra fetch needed)
  patchSettings: async (patch) => {
    try {
      const res = await fetch("/api/settings", {
        method: "PATCH",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(patch),
      });
      if (!res.ok) return null;
      const updated = await res.json();
      set({ settings: updated, lastFetched: Date.now() });
      return updated;
    } catch {
      return null;
    }
  },
}));

/**
 * React hook that ensures settings are loaded and re-renders the caller
 * when they arrive. Returns the cached settings (or null while loading).
 */
export function useEnsureSettings(): Record<string, any> | null {
  const settings = useSettingsStore((s) => s.settings);
  const loading = useSettingsStore((s) => s.loading);
  if (!settings && !loading && typeof window !== "undefined") {
    useSettingsStore.getState().fetchSettings();
  }
  return settings;
}
