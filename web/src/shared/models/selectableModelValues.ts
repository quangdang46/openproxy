// "Add All Active Models" model-set resolution (9router v0.5.116 #5449,
// commit 69d51c0c `src/shared/utils/selectableModels.js`).
//
// Resolves the same values the ModelSelectModal picker shows — no-auth
// free providers, user aliases, custom models, live per-account catalogs —
// minus disabled models, so callers that bulk-add models (Pi CLI tool card)
// never drift from the picker. LLM lane only (kindFilter = null), matching
// the picker's default.

import { getModelsByProviderId, useEnsureCatalog } from "@/shared/constants/models";
import {
  OAUTH_PROVIDERS,
  APIKEY_PROVIDERS,
  FREE_PROVIDERS,
  FREE_TIER_PROVIDERS,
  AI_PROVIDERS,
  isOpenAICompatibleProvider,
  isAnthropicCompatibleProvider,
  getProviderAlias,
} from "@/shared/constants/providers";
import {
  buildAvailableModels,
  fetchLiveModels,
  loadFreeOnly,
  type LiveModel,
} from "@/shared/models/availableModels";
import { useEffect, useMemo, useState } from "react";

const ALL_PROVIDERS = {
  ...OAUTH_PROVIDERS,
  ...FREE_PROVIDERS,
  ...FREE_TIER_PROVIDERS,
  ...APIKEY_PROVIDERS,
};

const PROVIDER_ORDER = [
  ...Object.keys(OAUTH_PROVIDERS),
  ...Object.keys(FREE_PROVIDERS),
  ...Object.keys(FREE_TIER_PROVIDERS),
  ...Object.keys(APIKEY_PROVIDERS),
];

const NO_AUTH_PROVIDER_IDS = Object.keys(FREE_PROVIDERS).filter(
  (id) => FREE_PROVIDERS[id].noAuth
);

interface ActiveProvider {
  provider: string;
  name?: string;
  providerSpecificData?: Record<string, any>;
  [key: string]: any;
}

interface PickerData {
  modelAliases: Record<string, string>;
  providerNodes: Array<{ id: string; name?: string; prefix?: string }>;
  customModels: Array<{ id: string; name?: string; providerAlias: string; kind?: string; type?: string }>;
  disabledMap: Record<string, string[]>;
  liveModelsByAlias: Record<string, LiveModel[]>;
  freeOnlyByAlias: Record<string, boolean>;
  ready: boolean;
}

/** Fetch the same runtime data ModelSelectModal loads when it opens. */
function usePickerData(enabled: boolean): PickerData {
  const [modelAliases, setModelAliases] = useState<Record<string, string>>({});
  const [providerNodes, setProviderNodes] = useState<any[]>([]);
  const [customModels, setCustomModels] = useState<any[]>([]);
  const [disabledMap, setDisabledMap] = useState<Record<string, string[]>>({});
  const [liveModelsByAlias, setLiveModelsByAlias] = useState<Record<string, LiveModel[]>>({});
  const [freeOnlyByAlias, setFreeOnlyByAlias] = useState<Record<string, boolean>>({});
  const [ready, setReady] = useState(false);

  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    (async () => {
      const [aliasesRes, nodesRes, customRes, disabledRes] = await Promise.all([
        fetch("/api/models/alias", { cache: "no-store" }).catch(() => null),
        fetch("/api/provider-nodes").catch(() => null),
        fetch("/api/models/custom", { cache: "no-store" }).catch(() => null),
        fetch("/api/models/disabled", { cache: "no-store" }).catch(() => null),
      ]);
      if (cancelled) return;
      if (aliasesRes?.ok) {
        const d = await aliasesRes.json().catch(() => null);
        if (d?.aliases && typeof d.aliases === "object") setModelAliases(d.aliases);
      }
      if (nodesRes?.ok) {
        const d = await nodesRes.json().catch(() => null);
        setProviderNodes(d?.nodes || []);
      }
      if (customRes?.ok) {
        const d = await customRes.json().catch(() => null);
        setCustomModels(d?.models || []);
      }
      if (disabledRes?.ok) {
        const d = await disabledRes.json().catch(() => null);
        setDisabledMap(d?.disabled && typeof d.disabled === "object" ? d.disabled : {});
      }

      const liveCapable = Object.entries(AI_PROVIDERS)
        .filter(([id, p]) => id === "kilocode" || !!p.modelsFetcher)
        .map(([id]) => id);
      const liveEntries = await Promise.all(
        liveCapable.map(async (id) => {
          const alias = getProviderAlias(id);
          return [alias, await fetchLiveModels(id, alias)] as const;
        })
      );
      if (cancelled) return;
      const liveMap: Record<string, LiveModel[]> = {};
      for (const [alias, models] of liveEntries) liveMap[alias] = models;
      setLiveModelsByAlias(liveMap);

      const aliases = [...new Set(Object.keys(AI_PROVIDERS).map((id) => getProviderAlias(id)))];
      const freeEntries = await Promise.all(
        aliases.map(async (alias) => [alias, await loadFreeOnly(alias)] as const)
      );
      if (cancelled) return;
      const freeMap: Record<string, boolean> = {};
      for (const [alias, v] of freeEntries) freeMap[alias] = v;
      setFreeOnlyByAlias(freeMap);
      setReady(true);
    })();
    return () => {
      cancelled = true;
    };
  }, [enabled]);

  return { modelAliases, providerNodes, customModels, disabledMap, liveModelsByAlias, freeOnlyByAlias, ready };
}

/**
 * Values ("alias/model-id") for every model the picker would show for the
 * currently active providers — including no-auth providers, aliases and
 * custom models — with disabled models excluded.
 */
export function useAllActiveModelValues(
  activeProviders: ActiveProvider[] | undefined,
  allConnections?: ActiveProvider[]
): { values: string[]; ready: boolean } {
  const catalogReady = useEnsureCatalog();
  const data = usePickerData(!!activeProviders?.length);

  return useMemo(() => {
    if (!activeProviders?.length || !data.ready || !catalogReady) {
      return { values: [], ready: false };
    }
    const connections = allConnections ?? activeProviders;
    // Paused providers (every connection inactive) drop out — same rule as
    // the picker via getDeactivatedProviderIds; approximated here by the
    // active-only id set plus no-auth providers that never own a row.
    const activeIds = new Set(activeProviders.map((p) => p.provider));
    const noAuthIds = NO_AUTH_PROVIDER_IDS.filter((id) => !activeIds.has(id) || true);
    const providerIds = new Set([...activeIds, ...noAuthIds]);
    // hidden providers never surface (devin-cli, mimo-free).
    for (const id of [...providerIds]) {
      if (AI_PROVIDERS[id]?.hidden) providerIds.delete(id);
    }
    const sorted = [...providerIds].sort((a, b) => {
      const ia = PROVIDER_ORDER.indexOf(a);
      const ib = PROVIDER_ORDER.indexOf(b);
      return (ia === -1 ? 999 : ia) - (ib === -1 ? 999 : ib);
    });

    const values: string[] = [];
    const seen = new Set<string>();
    const push = (v?: string) => {
      if (v && !seen.has(v)) {
        seen.add(v);
        values.push(v);
      }
    };

    for (const providerId of sorted) {
      const alias = getProviderAlias(providerId);
      const providerInfo = ALL_PROVIDERS[providerId] as any;
      if (!providerInfo) continue;
      const isCustomProvider =
        isOpenAICompatibleProvider(providerId) || isAnthropicCompatibleProvider(providerId);
      const disabledIds = [
        ...(data.disabledMap[alias] || []),
        ...(data.disabledMap[providerId] || []),
      ];

      if (providerInfo.passthroughModels) {
        const built = buildAvailableModels({
          catalogModels: getModelsByProviderId(providerId) as any,
          liveModels: data.liveModelsByAlias[alias] || [],
          customModels: data.customModels as any,
          modelAliases: data.modelAliases,
          disabledIds,
          providerAlias: alias,
          type: "llm",
          freeOnly: data.freeOnlyByAlias[alias] || false,
        });
        if (built.enabledRows.length > 0) {
          built.enabledRows.forEach((r) => push(r.fullModel));
        } else if (built.allRows.length === 0) {
          // Provider-as-model fallback (bare alias), same as the picker.
          push(alias);
        }
      } else if (isCustomProvider) {
        const connection =
          activeProviders.find((p) => p.provider === providerId) ||
          connections.find((p) => p.provider === providerId);
        const matchedNode = data.providerNodes.find((n) => n.id === providerId);
        const nodePrefix = connection?.providerSpecificData?.prefix || matchedNode?.prefix || providerId;
        const nodeModels = Object.entries(data.modelAliases)
          .filter(([, full]) => full.startsWith(`${providerId}/`))
          .map(([aliasName, full]) => ({
            id: full.replace(`${providerId}/`, ""),
            name: aliasName,
            value: `${nodePrefix}/${full.replace(`${providerId}/`, "")}`,
          }));
        const registered = data.customModels
          .filter((m) => m.providerAlias === providerId)
          .map((m) => ({ value: `${nodePrefix}/${m.id}` }));
        const seenVals = new Set(nodeModels.map((m) => m.value));
        [...nodeModels, ...registered.filter((m) => !seenVals.has(m.value))].forEach((m) =>
          push((m as any).value)
        );
        if (nodeModels.length === 0 && registered.length === 0) {
          push(`${nodePrefix}/model-id`);
        }
      } else {
        const built = buildAvailableModels({
          catalogModels: getModelsByProviderId(providerId) as any,
          liveModels: data.liveModelsByAlias[alias] || [],
          customModels: data.customModels as any,
          modelAliases: data.modelAliases,
          disabledIds,
          providerAlias: alias,
          type: "llm",
          freeOnly: data.freeOnlyByAlias[alias] || false,
        });
        if (built.enabledRows.length > 0) {
          built.enabledRows.forEach((r) => push(r.fullModel));
        } else if (built.allRows.length === 0) {
          push(alias);
        }
      }
    }
    return { values, ready: true };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeProviders, allConnections, data, catalogReady]);
}
