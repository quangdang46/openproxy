"use client";

import { useEffect, useState } from "react";
import { Card, Badge, Button, Toggle, AddCustomEmbeddingModal } from "@/shared/components";
import ProviderIcon from "@/shared/components/ProviderIcon";
import { MEDIA_PROVIDER_KINDS, AI_PROVIDERS, getProvidersByKind } from "@/shared/constants/providers";
import React from "react";

const COMBO_KINDS = new Set([]);
const COMBO_BASE_NAMES: Record<string, string> = { image: "image-combo", tts: "tts-combo" };

function getEffectiveStatus(conn: any) {
  const isCooldown = Object.entries(conn).some(
    ([k, v]) => k.startsWith("modelLock_") && v && new Date(v as string).getTime() > Date.now()
  );
  return conn.testStatus === "unavailable" && !isCooldown ? "active" : conn.testStatus;
}

function MediaProviderCard({
  provider,
  kind,
  connections,
  isCustom,
  onToggle,
}: {
  provider: any;
  kind: string;
  connections: any[];
  isCustom?: boolean;
  onToggle?: (providerId: string, allDisabled: boolean) => void;
}) {
  const providerInfo = AI_PROVIDERS[provider.id];
  const isNoAuth = !!providerInfo?.noAuth;

  const providerConns = connections.filter((c: any) => c.provider === provider.id);
  const connected = providerConns.filter((c: any) => { const s = getEffectiveStatus(c); return s === "active" || s === "success"; }).length;
  const error = providerConns.filter((c: any) => { const s = getEffectiveStatus(c); return s === "error" || s === "expired" || s === "unavailable"; }).length;
  const total = providerConns.length;
  const allDisabled = total > 0 && providerConns.every((c: any) => c.isActive === false);

  const handleToggleClick = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (onToggle) onToggle(provider.id, allDisabled);
  };

  return (
    <a href={`/dashboard/media-providers/${kind}/${provider.id}`} className="group">
      <Card
        padding="xs"
        className={`h-full hover:bg-black/[0.01] dark:hover:bg-white/[0.01] transition-colors cursor-pointer ${allDisabled ? "opacity-50" : ""}`}
      >
        <div className="flex min-w-0 items-center justify-between gap-3">
          <div className="flex min-w-0 items-center gap-3">
            <div
              className="size-8 rounded-mini-lg flex items-center justify-center shrink-0"
              style={{ backgroundColor: `${(provider.color?.length > 7 ? provider.color : (provider.color ?? "#888"))}15` }}
            >
              <ProviderIcon
                src={`/providers/${provider.id}.png`}
                alt={provider.name}
                size={30}
                className="object-contain rounded-mini-lg max-w-[30px] max-h-[30px]"
                fallbackText={provider.textIcon || provider.id.slice(0, 2).toUpperCase()}
                fallbackColor={provider.color}
              />
            </div>
            <div className="min-w-0">
              <h3 className="font-semibold text-sm">{provider.name}</h3>
              <div className="flex items-center gap-2 mt-0.5 flex-wrap">
                {isCustom && <Badge variant="default" size="sm">Custom</Badge>}
                {(() => {
                  if (isNoAuth) return <Badge variant="success" size="sm">Ready</Badge>;
                  if (allDisabled) return <Badge variant="default" size="sm">Disabled</Badge>;
                  if (total === 0) return <span className="text-xs text-text-muted">No connections</span>;
                  return (
                    <>
                      {connected > 0 && <Badge variant="success" size="sm" dot>{connected} Connected</Badge>}
                      {error > 0 && <Badge variant="error" size="sm" dot>{error} Error</Badge>}
                      {connected === 0 && error === 0 && <Badge variant="default" size="sm">{total} Added</Badge>}
                    </>
                  );
                })()}
              </div>
            </div>
          </div>
          {total > 0 && (
            <div
              className="shrink-0 opacity-100 transition-opacity sm:opacity-0 sm:group-hover:opacity-100"
              onClick={handleToggleClick}
            >
              <Toggle
                size="sm"
                checked={!allDisabled}
                onChange={() => {}}
              />
            </div>
          )}
        </div>
      </Card>
    </a>
  );
}

function ComboList({ combos }: { combos: any[] }) {
  if (combos.length === 0) return null;
  return (
    <div className="flex flex-col gap-2">
      {combos.map((combo: any) => (
        <a key={combo.id} href={`/dashboard/media-providers/combo/${combo.id}`}>
          <Card padding="xs" className="hover:bg-black/[0.02] dark:hover:bg-white/[0.02] transition-colors cursor-pointer">
            <div className="flex min-w-0 items-center gap-3">
              <span className="material-symbols-outlined text-brand-coral text-[18px]">layers</span>
              <code className="text-sm font-mono font-medium flex-1 truncate">{combo.name}</code>
              <div className="flex flex-wrap items-center gap-1 sm:shrink-0">
                {(combo.models || []).slice(0, 6).map((entry: any, i: number) => {
                  const pid = typeof entry === "string" ? entry.split("/")[0] : "";
                  const p = AI_PROVIDERS[pid];
                  return (
                    <div key={`${entry}-${i}`} title={p?.name || entry} className="size-5 rounded flex items-center justify-center" style={{ backgroundColor: `${(p?.color ?? "#888")}15` }}>
                      <ProviderIcon
                        src={`/providers/${pid}.png`}
                        alt={p?.name || pid}
                        size={18}
                        className="object-contain rounded max-w-[18px] max-h-[18px]"
                        fallbackText={p?.textIcon || pid.slice(0, 2).toUpperCase()}
                        fallbackColor={p?.color}
                      />
                    </div>
                  );
                })}
                {(combo.models || []).length > 6 && (
                  <span className="text-[10px] text-text-muted ml-1">+{(combo.models || []).length - 6}</span>
                )}
              </div>
              <span className="text-[11px] text-text-muted shrink-0">{(combo.models || []).length}</span>
              <span className="material-symbols-outlined text-text-muted text-[16px]">chevron_right</span>
            </div>
          </Card>
        </a>
      ))}
    </div>
  );
}

export default function MediaProvidersKindPageClient() {
  const [kind, setKind] = useState<string>("");
  const [connections, setConnections] = useState<any[]>([]);
  const [customNodes, setCustomNodes] = useState<any[]>([]);
  const [combos, setCombos] = useState<any[]>([]);
  const [showAddCustomEmbedding, setShowAddCustomEmbedding] = useState(false);

  useEffect(() => {
    const pathParts = window.location.pathname.split("/");
    const kindFromPath = pathParts[pathParts.length - 1];
    setKind(kindFromPath);

    // Redirect webSearch/webFetch to /web
    if (kindFromPath === "webSearch" || kindFromPath === "webFetch") {
      window.location.href = "/dashboard/media-providers/web";
      return;
    }
  }, []);

  const kindConfig = MEDIA_PROVIDER_KINDS.find((k) => k.id === kind);
  const isEmbedding = kind === "embedding";
  const supportsCombo = COMBO_KINDS.has(kind);

  useEffect(() => {
    if (!kindConfig) return;
    fetch("/api/providers", { cache: "no-store" })
      .then((r) => r.json())
      .then((d: any) => setConnections(d.connections || []))
      .catch(() => {});
    if (isEmbedding) {
      fetch("/api/provider-nodes", { cache: "no-store" })
        .then((r) => r.json())
        .then((d: any) => setCustomNodes((d.nodes || []).filter((n: any) => n.type === "custom-embedding")))
        .catch(() => {});
    }
    if (supportsCombo) {
      fetch("/api/combos", { cache: "no-store" })
        .then((r) => r.json())
        .then((d: any) => setCombos(d.combos || []))
        .catch(() => {});
    }
  }, [isEmbedding, supportsCombo, kindConfig]);

  if (!kindConfig) {
    return (
      <div className="flex flex-col gap-6">
        <h1 className="text-2xl font-bold">Kind Not Found</h1>
        <Card padding="lg">
          <p className="text-text-muted">The requested provider kind could not be found.</p>
        </Card>
      </div>
    );
  }

  const providers = getProvidersByKind(kind as any);
  const kindCombos = combos.filter((c: any) => c.kind === kind);

  const customProviders = customNodes.map((n: any) => ({
    id: n.id,
    name: n.name || "Custom Embedding",
    color: "#6366F1",
    textIcon: "CE",
  }));

  const allProviders = [...providers, ...customProviders];

  const handleToggleProvider = async (providerId: string, newActive: boolean) => {
    const providerConns = connections.filter((c: any) => c.provider === providerId);
    setConnections((prev: any[]) =>
      prev.map((c: any) => (c.provider === providerId ? { ...c, isActive: newActive } : c))
    );
    await Promise.allSettled(
      providerConns.map((c: any) =>
        fetch(`/api/providers/${c.id}`, {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ isActive: newActive }),
        })
      )
    );
  };

  const handleCreateCombo = async () => {
    const base = COMBO_BASE_NAMES[kind] || `${kind}-combo`;
    let name = base;
    let i = 1;
    const existing = new Set(combos.map((c: any) => c.name));
    while (existing.has(name)) { name = `${base}-${i++}`; }
    const res = await fetch("/api/combos", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name, models: [], kind }),
    });
    if (res.ok) {
      const created = await res.json();
      window.location.href = `/dashboard/media-providers/combo/${created.id}`;
    } else {
      const err = await res.json();
      alert(err.error || "Failed to create combo");
    }
  };

  return (
    <div className="flex flex-col gap-6">
      {(isEmbedding || supportsCombo) && (
        <div className="flex items-center justify-end gap-2">
          {supportsCombo && (
            <Button size="sm" icon="add" onClick={handleCreateCombo}>Create Combo</Button>
          )}
          {isEmbedding && (
            <Button size="sm" icon="add" onClick={() => setShowAddCustomEmbedding(true)}>
              Add Custom Embedding
            </Button>
          )}
        </div>
      )}

      {supportsCombo && kindCombos.length > 0 && (
        <ComboList combos={kindCombos} />
      )}

      {allProviders.length === 0 ? (
        <div className="text-center py-12 border border-dashed border-hairline rounded-mini-xl text-text-muted text-sm">
          No providers support <strong>{kindConfig.label}</strong> yet.
        </div>
      ) : (
        <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4 gap-4">
          {providers.map((provider: any) => (
            <MediaProviderCard
              key={provider.id}
              provider={provider}
              kind={kind}
              connections={connections}
              onToggle={(pid: string, disabled: boolean) => handleToggleProvider(pid, disabled)}
            />
          ))}
          {customProviders.map((provider: any) => (
            <MediaProviderCard
              key={provider.id}
              provider={provider}
              kind={kind}
              connections={connections}
              isCustom
              onToggle={(pid: string, disabled: boolean) => handleToggleProvider(pid, disabled)}
            />
          ))}
        </div>
      )}

      {isEmbedding && (
        <AddCustomEmbeddingModal
          isOpen={showAddCustomEmbedding}
          onClose={() => setShowAddCustomEmbedding(false)}
          onCreated={(node: any) => {
            setCustomNodes((prev: any[]) => [...prev, node]);
            setShowAddCustomEmbedding(false);
          }}
        />
      )}
    </div>
  );
}
