import { UPDATER_CONFIG } from "@/shared/constants/config";

export interface EndpointPreset {
  name: string;
  baseUrl: string;
}

export interface ApiKeyPreset {
  name: string;
  key: string;
}

interface StoreConfig {
  storageKey: string;
  changeEvent: string;
  itemField: string;
  normalize?: (value: string) => string;
  defaultName?: (value: string) => string;
}

// A record as it sits in localStorage: a name plus the one field the store owns.
// The field name is only known at runtime, hence the read through a map.
type StoreItem = { name: string };

export interface Store<T extends StoreItem> {
  read: () => T[];
  subscribe: (handler: () => void) => () => void;
  // Adds or replaces a preset; returns the stored name, or null when skipped.
  upsert: (value: string, name?: string | null) => string | null;
  remove: (name: string) => void;
}

// Browser-local preset stores (endpoints, API keys) shared by every CLI tool
// card. Each store owns its own localStorage key and change event so the two
// never contend for one another's records.
function fieldOf(p: StoreItem): Record<string, string> {
  return p as unknown as Record<string, string>;
}

function createStore<T extends StoreItem>({
  storageKey,
  changeEvent,
  itemField,
  normalize = (v) => v,
  defaultName = (v) => v,
}: StoreConfig): Store<T> {
  const read = (): T[] => {
    if (typeof window === "undefined") return [];
    try {
      const raw = JSON.parse(window.localStorage.getItem(storageKey) || "[]");
      if (!Array.isArray(raw)) return [];
      return raw.filter((p: StoreItem) => p?.name && fieldOf(p)[itemField]);
    } catch {
      return [];
    }
  };

  const write = (items: T[]): void => {
    if (typeof window === "undefined") return;
    window.localStorage.setItem(storageKey, JSON.stringify(items));
    window.dispatchEvent(new CustomEvent(changeEvent));
  };

  return {
    read,
    subscribe: (handler) => {
      if (typeof window === "undefined") return () => {};
      window.addEventListener(changeEvent, handler);
      return () => window.removeEventListener(changeEvent, handler);
    },
    upsert: (value: string, name?: string | null): string | null => {
      const v = normalize(value);
      if (!v) return null;

      const items = read();
      const existing = items.find((p) => normalize(fieldOf(p)[itemField]) === v);
      if (existing && !name) return existing.name;

      const finalName = (name || defaultName(v)).trim();
      if (!finalName) return null;

      const next = [
        ...items.filter((p) => p.name !== finalName && normalize(fieldOf(p)[itemField]) !== v),
        { name: finalName, [itemField]: v },
      ].sort((a, b) => a.name.localeCompare(b.name));
      write(next as T[]);
      return finalName;
    },
    remove: (name) => write(read().filter((p) => p.name !== name)),
  };
}

export const stripSlash = (url: string): string => (url || "").replace(/\/+$/, "");

const endpoints = createStore<EndpointPreset>({
  storageKey: "openproxy.cliToolEndpointPresets",
  changeEvent: "openproxy:endpoint-presets-changed",
  itemField: "baseUrl",
  normalize: stripSlash,
  defaultName: (url) => {
    try { return new URL(url).host; } catch { return url; }
  },
});

const apiKeys = createStore<ApiKeyPreset>({
  storageKey: "openproxy.cliToolApiKeyPresets",
  changeEvent: "openproxy:api-key-presets-changed",
  itemField: "key",
});

export const readPresets = endpoints.read;
export const subscribePresets = endpoints.subscribe;
export const upsertPreset = endpoints.upsert;
export const deletePreset = endpoints.remove;

export const readKeyPresets = apiKeys.read;
export const subscribeKeyPresets = apiKeys.subscribe;
export const upsertKeyPreset = apiKeys.upsert;
export const deleteKeyPreset = apiKeys.remove;

// Save an applied endpoint unless it exactly matches a built-in dropdown option.
export function rememberEndpoint(
  baseUrl: string,
  { tunnelPublicUrl, tailscaleUrl, cloudUrl }: {
    tunnelPublicUrl?: string;
    tailscaleUrl?: string;
    cloudUrl?: string;
  } = {}
): string | null {
  const url = stripSlash(baseUrl);
  if (!url) return null;

  const builtIns = [`http://127.0.0.1:${UPDATER_CONFIG.appPort}`, tunnelPublicUrl, tailscaleUrl, cloudUrl]
    .filter(Boolean)
    .flatMap((u) => [stripSlash(u as string), `${stripSlash(u as string)}/v1`]);
  if (builtIns.includes(url)) return null;

  return upsertPreset(url);
}
