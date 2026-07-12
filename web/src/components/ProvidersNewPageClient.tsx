"use client";

import { useMemo, useState } from "react";
import { Card, Button, Input, Select } from "@/shared/components";
import ProviderIcon from "@/shared/components/ProviderIcon";
import {
  OAUTH_PROVIDERS,
  APIKEY_PROVIDERS,
  FREE_PROVIDERS,
  FREE_TIER_PROVIDERS,
  WEB_COOKIE_PROVIDERS,
} from "@/shared/constants/providers";
import type { Provider } from "@/types";
import { useNotificationStore } from "@/store/notificationStore";

type GroupId =
  | "oauth"
  | "free"
  | "freeTier"
  | "apikey"
  | "webCookie"
  | "compatible";

type Selectable =
  | {
      kind: "provider";
      group: Exclude<GroupId, "compatible">;
      id: string;
      provider: Provider;
    }
  | {
      kind: "compatible";
      group: "compatible";
      id: "openai-compatible" | "anthropic-compatible";
      name: string;
    };

const GROUP_META: Record<GroupId, { label: string; hint: string }> = {
  oauth: {
    label: "OAuth",
    hint: "Connect with your subscription account via browser OAuth.",
  },
  free: {
    label: "Free",
    hint: "Free-tier OAuth / no-auth providers.",
  },
  freeTier: {
    label: "Free Tier (API Key)",
    hint: "Providers with a free API-key tier.",
  },
  apikey: {
    label: "API Key",
    hint: "Paste an API key here, or open the provider list to add it there.",
  },
  webCookie: {
    label: "Web Cookie",
    hint: "Use a browser session cookie from a subscription site.",
  },
  compatible: {
    label: "Compatible Endpoint",
    hint: "Add a custom OpenAI- or Anthropic-compatible base URL from the providers list.",
  },
};

function isLlm(provider: Provider): boolean {
  return (provider.serviceKinds ?? ["llm"]).includes("llm");
}

function toOptions(
  group: Exclude<GroupId, "compatible">,
  map: Record<string, Provider>,
): Selectable[] {
  return Object.entries(map)
    .filter(([, p]) => !p.hidden && isLlm(p))
    .sort(([, a], [, b]) => {
      const pa = a.priority ?? 999;
      const pb = b.priority ?? 999;
      if (pa !== pb) return pa - pb;
      if (!!a.noAuth !== !!b.noAuth) return a.noAuth ? -1 : 1;
      return (a.name || "").localeCompare(b.name || "");
    })
    .map(([id, provider]) => ({
      kind: "provider" as const,
      group,
      id,
      provider,
    }));
}

export default function ProvidersNewPageClient() {
  const notify = useNotificationStore();
  const [selectedKey, setSelectedKey] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [saving, setSaving] = useState(false);

  const selectables = useMemo<Selectable[]>(() => {
    return [
      ...toOptions("oauth", OAUTH_PROVIDERS),
      ...toOptions("free", FREE_PROVIDERS),
      ...toOptions("freeTier", FREE_TIER_PROVIDERS),
      ...toOptions("apikey", APIKEY_PROVIDERS),
      ...toOptions("webCookie", WEB_COOKIE_PROVIDERS),
      {
        kind: "compatible",
        group: "compatible",
        id: "openai-compatible",
        name: "OpenAI Compatible (custom endpoint)",
      },
      {
        kind: "compatible",
        group: "compatible",
        id: "anthropic-compatible",
        name: "Anthropic Compatible (custom endpoint)",
      },
    ];
  }, []);

  const selected = useMemo(
    () => selectables.find((s) => `${s.group}:${s.id}` === selectedKey) || null,
    [selectables, selectedKey],
  );

  const selectOptions = useMemo(() => {
    const byGroup = new Map<GroupId, Selectable[]>();
    for (const item of selectables) {
      const list = byGroup.get(item.group) || [];
      list.push(item);
      byGroup.set(item.group, list);
    }
    const options: { value: string; label: string }[] = [];
    (Object.keys(GROUP_META) as GroupId[]).forEach((group) => {
      const items = byGroup.get(group);
      if (!items?.length) return;
      for (const item of items) {
        const name = item.kind === "provider" ? item.provider.name : item.name;
        options.push({
          value: `${item.group}:${item.id}`,
          label: `[${GROUP_META[group].label}] ${name}`,
        });
      }
    });
    return options;
  }, [selectables]);

  const isApiKeyGroup =
    selected?.kind === "provider" &&
    (selected.group === "apikey" ||
      selected.group === "freeTier" ||
      selected.group === "webCookie");

  const isCookie =
    selected?.kind === "provider" &&
    (selected.group === "webCookie" || selected.provider.authType === "cookie");

  const isNoAuth =
    selected?.kind === "provider" && !!selected.provider.noAuth;

  const isOAuthLike =
    selected?.kind === "provider" &&
    (selected.group === "oauth" ||
      (selected.group === "free" && !selected.provider.noAuth));

  const startOAuth = (providerId: string) => {
    window.location.href = `/api/oauth/${encodeURIComponent(providerId)}/start`;
  };

  const goToListAdd = (providerId: string) => {
    window.location.href = `/dashboard/providers?add=${encodeURIComponent(providerId)}`;
  };

  const goToDetail = (providerId: string) => {
    window.location.href = `/dashboard/providers/${encodeURIComponent(providerId)}`;
  };

  const goToCompatible = (
    variant: "openai-compatible" | "anthropic-compatible",
  ) => {
    window.location.href = `/dashboard/providers?compatible=${
      variant === "openai-compatible" ? "openai" : "anthropic"
    }`;
  };

  const handleConnect = async () => {
    if (!selected) return;

    if (selected.kind === "compatible") {
      goToCompatible(selected.id);
      return;
    }

    const { id, provider, group } = selected;

    if (group === "oauth" || (group === "free" && !provider.noAuth)) {
      startOAuth(id);
      return;
    }

    if (provider.noAuth) {
      goToDetail(id);
      return;
    }

    if (isApiKeyGroup) {
      if (apiKey.trim()) {
        setSaving(true);
        try {
          const res = await fetch("/api/providers", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({
              provider: id,
              name: displayName.trim() || (isCookie ? "Cookie" : "API Key"),
              apiKey: apiKey.trim(),
              priority: 1,
              testStatus: "unknown",
            }),
          });
          if (!res.ok) {
            const data = await res.json().catch(() => ({}));
            notify.error(
              (data as { error?: string }).error ||
                "Failed to add provider key",
            );
            return;
          }
          notify.success(`${provider.name} connected`);
          goToDetail(id);
        } catch {
          notify.error("Failed to add provider key");
        } finally {
          setSaving(false);
        }
        return;
      }
      goToListAdd(id);
      return;
    }

    goToDetail(id);
  };

  const primaryLabel = (() => {
    if (!selected) return "Connect Provider";
    if (selected.kind === "compatible") return "Open Compatible Setup";
    if (isOAuthLike) return "Start OAuth";
    if (isNoAuth) return "Open Provider";
    if (isApiKeyGroup && apiKey.trim()) {
      return saving
        ? "Saving..."
        : isCookie
          ? "Save Cookie"
          : "Save API Key";
    }
    if (isApiKeyGroup) {
      return isCookie ? "Add Cookie on List" : "Add Key on List";
    }
    return "Continue";
  })();

  return (
    <div className="mx-auto flex w-full max-w-2xl flex-col gap-6">
      <div>
        <a
          href="/dashboard/providers"
          className="mb-4 inline-flex items-center gap-1 text-sm text-text-muted transition-colors hover:text-primary"
        >
          <span className="material-symbols-outlined text-lg">arrow_back</span>
          Back to Providers
        </a>
        <h1 className="text-2xl font-bold tracking-tight sm:text-3xl">
          Add New Provider
        </h1>
        <p className="mt-2 text-sm text-text-muted">
          Pick a provider, then connect with OAuth, an API key, a free tier, a
          web cookie, or a custom compatible endpoint.
        </p>
      </div>

      <Card padding="lg">
        <div className="flex flex-col gap-5">
          <Select
            label="Provider"
            required
            placeholder="Choose a provider..."
            options={selectOptions}
            value={selectedKey}
            onChange={(e) => {
              setSelectedKey(e.target.value);
              setApiKey("");
              setDisplayName("");
            }}
            hint="Grouped as OAuth · Free · Free Tier · API Key · Web Cookie · Compatible"
          />

          {selected?.kind === "provider" && (
            <div className="flex items-center gap-3 rounded-xl border border-border bg-bg-subtle/40 p-3">
              <div
                className="flex size-10 shrink-0 items-center justify-center rounded-lg"
                style={{
                  backgroundColor:
                    selected.provider.color?.length > 7
                      ? selected.provider.color
                      : `${selected.provider.color || "#888"}15`,
                }}
              >
                <ProviderIcon
                  src={`/providers/${selected.provider.id}.png`}
                  alt={selected.provider.name}
                  size={28}
                  className="max-h-[28px] max-w-[28px] rounded-lg object-contain"
                  fallbackText={
                    selected.provider.textIcon ||
                    selected.provider.id.slice(0, 2).toUpperCase()
                  }
                  fallbackColor={selected.provider.color}
                />
              </div>
              <div className="min-w-0">
                <p className="truncate font-medium">{selected.provider.name}</p>
                <p className="text-xs text-text-muted">
                  {GROUP_META[selected.group].hint}
                  {selected.provider.noAuth ? " · No auth required" : ""}
                  {selected.provider.deprecated ? " · Deprecated" : ""}
                </p>
              </div>
            </div>
          )}

          {selected?.kind === "compatible" && (
            <div className="rounded-xl border border-dashed border-border p-3 text-sm text-text-muted">
              {GROUP_META.compatible.hint} You will be taken to the providers
              list with the add modal open.
            </div>
          )}

          {isOAuthLike && selected?.kind === "provider" && (
            <div className="rounded-xl border border-border bg-primary/5 p-3 text-sm text-text-muted">
              You will be redirected to complete OAuth for{" "}
              <span className="font-medium text-text-main">
                {selected.provider.name}
              </span>
              .
              {selected.id === "kiro" && (
                <span className="mt-1 block text-xs">
                  Kiro also supports API-key auth from the provider detail page.
                </span>
              )}
            </div>
          )}

          {isApiKeyGroup && selected?.kind === "provider" && (
            <div className="flex flex-col gap-3">
              <Input
                label="Display Name"
                placeholder={
                  isCookie ? "e.g. Personal cookie" : "e.g. Production key"
                }
                value={displayName}
                onChange={(e) => setDisplayName(e.target.value)}
                hint="Optional. Defaults to a generic label."
              />
              <Input
                label={isCookie ? "Cookie Value" : "API Key"}
                type={isCookie ? "text" : "password"}
                placeholder={
                  isCookie
                    ? selected.provider.authHint || "Paste cookie value"
                    : "sk-..."
                }
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
                hint={
                  apiKey.trim()
                    ? "Key will be saved on this page."
                    : "Leave blank to open the providers list with this provider pre-selected (?add=)."
                }
              />
              {(selected.provider.notice?.apiKeyUrl ||
                selected.provider.website) && (
                <a
                  href={
                    selected.provider.notice?.apiKeyUrl ||
                    selected.provider.website
                  }
                  target="_blank"
                  rel="noopener noreferrer"
                  className="text-xs text-primary underline"
                >
                  {selected.provider.notice?.apiKeyUrl
                    ? "Get API key"
                    : "Open provider site"}
                </a>
              )}
            </div>
          )}

          <div className="flex flex-col gap-2 border-t border-border pt-4 sm:flex-row">
            <a href="/dashboard/providers" className="flex-1">
              <Button type="button" variant="ghost" fullWidth>
                Cancel
              </Button>
            </a>
            <Button
              onClick={() => void handleConnect()}
              disabled={!selected || saving}
              fullWidth
              className="flex-1"
              loading={saving}
            >
              {primaryLabel}
            </Button>
          </div>
        </div>
      </Card>
    </div>
  );
}
