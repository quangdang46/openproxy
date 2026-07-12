export interface CustomModelEntry {
  providerAlias?: string;
  id?: string;
  name?: string;
  type?: string;
  kind?: string;
  [key: string]: unknown;
}

export interface CustomModelRow {
  id: string;
  name?: string;
  alias?: string;
  fullModel: string;
  source: "custom" | "legacyAlias";
  type: string;
}

function modelType(model: CustomModelEntry | null | undefined): string {
  return model?.kind || model?.type || "llm";
}

/**
 * Build display rows for a provider's custom models, merging:
 * 1. Registered entries from /api/models/custom
 * 2. Legacy alias map entries that map to `${providerAlias}/…`
 *
 * Built-in catalog models are excluded. Prefer custom source over legacyAlias
 * when both cover the same full model id.
 */
export function getProviderCustomModelRows({
  customModels = [],
  modelAliases = {},
  providerAlias,
  builtInModels = [],
  type = "llm",
  includeLegacyAliases = true,
}: {
  customModels?: CustomModelEntry[];
  modelAliases?: Record<string, string>;
  providerAlias: string;
  builtInModels?: Array<{ id: string }>;
  type?: string;
  includeLegacyAliases?: boolean;
}): CustomModelRow[] {
  const builtInIds = new Set(builtInModels.map((model) => model.id));
  const seenFullModels = new Set<string>();
  const rows: CustomModelRow[] = [];

  for (const model of customModels) {
    if (!model?.id || model.providerAlias !== providerAlias) continue;
    const rowType = modelType(model);
    if (type && rowType !== type) continue;
    if (builtInIds.has(model.id)) continue;

    const fullModel = `${providerAlias}/${model.id}`;
    if (seenFullModels.has(fullModel)) continue;
    seenFullModels.add(fullModel);
    rows.push({
      id: model.id,
      name: model.name || model.id,
      fullModel,
      source: "custom",
      type: rowType,
    });
  }

  if (!includeLegacyAliases) return rows;

  const prefix = `${providerAlias}/`;
  for (const [alias, fullModel] of Object.entries(modelAliases || {})) {
    if (typeof fullModel !== "string" || !fullModel.startsWith(prefix)) continue;
    const id = fullModel.slice(prefix.length);
    if (!id || builtInIds.has(id) || seenFullModels.has(fullModel)) continue;

    seenFullModels.add(fullModel);
    rows.push({
      id,
      alias,
      fullModel,
      source: "legacyAlias",
      type: type || "llm",
    });
  }

  return rows;
}
