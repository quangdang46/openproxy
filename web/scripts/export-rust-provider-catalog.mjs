import { writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
// Find repo root by looking for Cargo.toml (Rust project marker)
let repoRoot = path.resolve(__dirname, "..");
while (!require("node:fs").existsSync(path.join(repoRoot, "Cargo.toml")) && repoRoot !== path.dirname(repoRoot)) {
  repoRoot = path.dirname(repoRoot);
}
const webRoot = path.join(repoRoot, "web");

import { PROVIDER_ID_TO_ALIAS, PROVIDER_MODELS } from path.join(webRoot, "open-sse/config/providerModels.js");
import { AI_PROVIDERS } from path.join(webRoot, "src/shared/constants/providers.js");

const MODEL_TYPE_TO_KIND = {
  image: "image",
  tts: "tts",
  embedding: "embedding",
  stt: "stt",
  imageToText: "imageToText",
};

const providerModels = Object.entries(PROVIDER_MODELS).map(([alias, models]) => ({
  alias,
  models: models.map((model) => ({
    id: model.id,
    name: model.name || model.id,
    kind: MODEL_TYPE_TO_KIND[model.type] || "llm",
  })),
}));

const providers = Object.entries(AI_PROVIDERS).map(([id, provider]) => ({
  id,
  alias: provider.alias || id,
  serviceKinds:
    Array.isArray(provider.serviceKinds) && provider.serviceKinds.length > 0
      ? provider.serviceKinds
      : ["llm"],
  ttsModels: Array.isArray(provider.ttsConfig?.models)
    ? provider.ttsConfig.models.map((model) => model.id).filter(Boolean)
    : [],
  embeddingModels: Array.isArray(provider.embeddingConfig?.models)
    ? provider.embeddingConfig.models.map((model) => model.id).filter(Boolean)
    : [],
  hasSearch: Boolean(provider.searchConfig),
  hasFetch: Boolean(provider.fetchConfig),
}));

const catalog = {
  providerIdToAlias: PROVIDER_ID_TO_ALIAS,
  providerModels,
  providers,
};

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const outputPath = path.resolve(repoRoot, "src/core/model/provider_catalog.json");

writeFileSync(outputPath, `${JSON.stringify(catalog, null, 2)}\n`);
console.log(`Wrote ${outputPath}`);
