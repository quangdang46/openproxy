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

import { AI_PROVIDERS, isOpenAICompatibleProvider } from "./providers.jsx";
// import { PROVIDER_MODELS as MODELS } from "open-sse/config/providerModels.jsx";

// Temporary stubs
export const PROVIDER_MODELS = {};
export const getProviderModels = () => [];
export const getDefaultModel = () => "";
export const isValidModelCore = () => true;
export const findModelName = () => "";
export const getModelTargetFormat = () => "";
export const getModelStrip = () => "";
export const PROVIDER_ID_TO_ALIAS = {};
export const getModelsByProviderId = () => [];
export const getModelUpstreamId = () => "";
export const getModelQuotaFamily = () => "";
export const MODELS = {};

// Providers that accept any model (passthrough)
const PASSTHROUGH_PROVIDERS = new Set(
  Object.entries(AI_PROVIDERS)
    .filter(([, p]) => p.passthroughModels)
    .map(([key]) => key)
);

// Wrap isValidModel with passthrough providers
export function isValidModel(aliasOrId, modelId) {
  if (isOpenAICompatibleProvider(aliasOrId)) return true;
  if (PASSTHROUGH_PROVIDERS.has(aliasOrId)) return true;
  const models = MODELS[aliasOrId];
  if (!models) return false;
  return models.some(m => m.id === modelId);
}

// Legacy AI_MODELS for backward compatibility
export const AI_MODELS = Object.entries(MODELS).flatMap(([alias, models]) =>
  models.map(m => ({ provider: alias, model: m.id, name: m.name }))
);
