// Import directly from file to avoid pulling in server-side dependencies via index.js
// export {
//   PROVIDER_MODELS,
//   getProviderModels,
//   getDefaultModel,
//   isValidModel as isValidModelCore,
//   findModelName,
//   getModelTargetFormat,
//   getModelStrip,
//   PROVIDER_ID_TO_ALIAS,
//   getModelsByProviderId,
//   getModelUpstreamId,
//   getModelQuotaFamily
// } from "open-sse/config/providerModels.jsx";

import { AI_PROVIDERS, isOpenAICompatibleProvider } from "./providers";
// import { PROVIDER_MODELS as MODELS } from "open-sse/config/providerModels.jsx";

import type { Model } from "../../types";

// Temporary stubs
export const PROVIDER_MODELS: Record<string, Model[]> = {};
export const getProviderModels = (): Model[] => [];
export const getDefaultModel = (): string => "";
export const isValidModelCore = (): boolean => true;
export const findModelName = (): string => "";
export const getModelTargetFormat = (): string => "";
export const getModelStrip = (): string => "";
export const PROVIDER_ID_TO_ALIAS: Record<string, string> = {};
export const getModelsByProviderId = (): Model[] => [];
export const getModelUpstreamId = (): string => "";
export const getModelQuotaFamily = (): string => "";
export const MODELS: Record<string, Model[]> = {};

// Providers that accept any model (passthrough)
const PASSTHROUGH_PROVIDERS: Set<string> = new Set(
  Object.entries(AI_PROVIDERS)
    .filter(([, p]) => p.passthroughModels)
    .map(([key]) => key)
);

// Wrap isValidModel with passthrough providers
export function isValidModel(aliasOrId: string, modelId: string): boolean {
  if (isOpenAICompatibleProvider(aliasOrId)) return true;
  if (PASSTHROUGH_PROVIDERS.has(aliasOrId)) return true;
  const models = MODELS[aliasOrId];
  if (!models) return false;
  return models.some(m => m.id === modelId);
}

// Interface for AI model entries
interface AIModelEntry {
  provider: string;
  model: string;
  name: string;
}

// Legacy AI_MODELS for backward compatibility
export const AI_MODELS: AIModelEntry[] = Object.entries(MODELS).flatMap(([alias, models]) =>
  models.map(m => ({ provider: alias, model: m.id, name: m.name }))
);
