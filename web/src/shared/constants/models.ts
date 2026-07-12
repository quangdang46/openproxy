// Model catalog accessors. The actual data is loaded asynchronously from the
// Rust backend via GET /api/catalog (see src/core/model/provider_catalog.json,
// served by api_catalog in src/server/api/mod.rs).
//
// useEnsureCatalog() is the React hook components call so they re-render when
// the data arrives. The plain accessors below read the latest snapshot.

import { AI_PROVIDERS, isOpenAICompatibleProvider } from "./providers";
import { useCatalogStore, useEnsureCatalog } from "@/store/catalogStore";
import type { Model } from "../../types";

export { useEnsureCatalog };

function snapshot() {
  return useCatalogStore.getState();
}

function aliasFor(providerId: string): string {
  const { providerIdToAlias } = snapshot();
  return providerIdToAlias[providerId] || providerId;
}

export function getModelsByProviderId(providerId: string): Model[] {
  const { modelsByAlias } = snapshot();
  const alias = aliasFor(providerId);
  return (modelsByAlias[alias] || []) as unknown as Model[];
}

export function getProviderModels(aliasOrId: string): Model[] {
  const { modelsByAlias, providerIdToAlias } = snapshot();
  // Treat the argument as either a provider id or alias.
  const alias = providerIdToAlias[aliasOrId] || aliasOrId;
  return (modelsByAlias[alias] || []) as unknown as Model[];
}

export function getDefaultModel(aliasOrId: string): string {
  const models = getProviderModels(aliasOrId);
  return models[0]?.id || "";
}

export function findModelName(aliasOrId: string, modelId: string): string {
  const found = getProviderModels(aliasOrId).find((m) => m.id === modelId);
  return found?.name || modelId;
}

export function getModelTargetFormat(_aliasOrId: string, _modelId: string): string {
  return "";
}

export function getModelStrip(_aliasOrId: string, _modelId: string): string[] {
  return [];
}

export function getModelUpstreamId(_aliasOrId: string, modelId: string): string {
  return modelId;
}

export function getModelQuotaFamily(_aliasOrId: string, _modelId: string): string {
  return "normal";
}

export const PROVIDER_MODELS: Record<string, Model[]> = new Proxy(
  {},
  {
    get: (_target, key: string) => {
      const { modelsByAlias } = snapshot();
      return (modelsByAlias[key] || []) as unknown as Model[];
    },
    ownKeys: () => Object.keys(snapshot().modelsByAlias),
    getOwnPropertyDescriptor: () => ({ enumerable: true, configurable: true }),
  }
) as Record<string, Model[]>;

export const PROVIDER_ID_TO_ALIAS: Record<string, string> = new Proxy(
  {},
  {
    get: (_target, key: string) => snapshot().providerIdToAlias[key],
    ownKeys: () => Object.keys(snapshot().providerIdToAlias),
    getOwnPropertyDescriptor: () => ({ enumerable: true, configurable: true }),
  }
) as Record<string, string>;

export const MODELS = PROVIDER_MODELS;

// Providers that accept any model (passthrough)
const PASSTHROUGH_PROVIDERS: Set<string> = new Set(
  Object.entries(AI_PROVIDERS)
    .filter(([, p]) => p.passthroughModels)
    .map(([key]) => key)
);

export function isValidModelCore(aliasOrId: string, modelId: string): boolean {
  const models = snapshot().modelsByAlias[aliasOrId];
  if (!models) return false;
  return models.some((m) => m.id === modelId);
}

export function isValidModel(aliasOrId: string, modelId: string): boolean {
  if (isOpenAICompatibleProvider(aliasOrId)) return true;
  if (PASSTHROUGH_PROVIDERS.has(aliasOrId)) return true;
  return isValidModelCore(aliasOrId, modelId);
}

interface AIModelEntry {
  provider: string;
  model: string;
  name: string;
}

// Legacy AI_MODELS for backward compatibility — built from the current snapshot.
export const AI_MODELS: AIModelEntry[] = (() => {
  const out: AIModelEntry[] = [];
  const { modelsByAlias } = snapshot();
  for (const [alias, models] of Object.entries(modelsByAlias)) {
    for (const m of models) out.push({ provider: alias, model: m.id, name: m.name || m.id });
  }
  return out;
})();

// ── Capacity badges ─────────────────────────────────────────────
// Metadata for UI badges — icon + label + color per capability.
// Keep in sync with 9router CAPACITY_META (search hidden until wired).

export type CapacityKey = "vision" | "reasoning";

export interface ModelCaps {
  vision?: boolean;
  search?: boolean;
  reasoning?: boolean;
  [key: string]: boolean | undefined;
}

export interface CapacityMetaEntry {
  icon: string;
  label: string;
  desc: string;
  color: string;
}

export const CAPACITY_META: Record<CapacityKey, CapacityMetaEntry> = {
  vision: {
    icon: "visibility",
    label: "Vision",
    desc: "Supports image input",
    color: "text-blue-500",
  },
  // search: temporarily hidden (feature not wired yet)
  reasoning: {
    icon: "neurology",
    label: "Reasoning",
    desc: "Supports reasoning / thinking",
    color: "text-amber-500",
  },
};
