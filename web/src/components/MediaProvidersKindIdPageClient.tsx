"use client";

import { useState, useEffect } from "react";
import { Card, Badge, Button, AddCustomEmbeddingModal, NoAuthProxyCard, ProviderInfoCard } from "@/shared/components";
import ProviderIcon from "@/shared/components/ProviderIcon";
import { MEDIA_PROVIDER_KINDS, AI_PROVIDERS, isCustomEmbeddingProvider } from "@/shared/constants/providers";
import ConnectionsCard from "@/components/providers/ConnectionsCard";
import ModelsCard from "@/components/providers/ModelsCard";
import { KIND_EXAMPLE_CONFIG } from "@/components/media-providers/exampleShared";
import { EmbeddingExampleCard } from "@/components/media-providers/EmbeddingExampleCard";
import { TtsExampleCard } from "@/components/media-providers/TtsExampleCard";
import { GenericExampleCard } from "@/components/media-providers/GenericExampleCard";
import { SttExampleCard } from "@/components/media-providers/SttExampleCard";
import React from "react";

export default function MediaProvidersKindIdPageClient() {
  const [pathParts, setPathParts] = useState<string[]>([]);
  const [kind, setKind] = useState<string>("");
  const [id, setId] = useState<string>("");
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    const parts = window.location.pathname.split("/");
    setPathParts(parts);
    setKind(parts[parts.length - 2]);
    setId(parts[parts.length - 1]);
    setLoaded(true);
  }, []);

  if (!loaded) return null;

  return <MediaProviderDetailPage kind={kind} id={id} />;
}

function MediaProviderDetailPage({ kind, id }: { kind: string; id: string }) {
  const kindConfig = MEDIA_PROVIDER_KINDS.find((k) => k.id === kind);
  const isCustom = isCustomEmbeddingProvider(id) && kind === "embedding";

  const handleDeleteCustom = async () => {
    if (!confirm("Delete this Custom Embedding node?")) return;
    try {
      const res = await fetch(`/api/provider-nodes/${id}`, { method: "DELETE" });
      if (res.ok) window.location.href = `/dashboard/media-providers/${kind}`;
    } catch (error) {
      console.log("Error deleting custom embedding node:", error);
    }
  };

  const [customNode, setCustomNode] = useState<any>(null);
  const [customLoading, setCustomLoading] = useState(isCustom);
  const [showEditModal, setShowEditModal] = useState(false);

  useEffect(() => {
    if (!isCustom) return;
    let cancelled = false;
    fetch("/api/provider-nodes", { cache: "no-store" })
      .then((r) => r.json())
      .then((d: any) => {
        if (cancelled) return;
        setCustomNode((d.nodes || []).find((n: any) => n.id === id) || null);
        setCustomLoading(false);
      })
      .catch(() => { if (!cancelled) setCustomLoading(false); });
    return () => { cancelled = true; };
  }, [id, isCustom]);

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

  const builtInProvider = AI_PROVIDERS[id];

  const provider = isCustom
    ? (customNode ? { id, name: customNode.name || "Custom Embedding", color: "#6366F1", textIcon: "CE" } : null)
    : builtInProvider;

  if (!isCustom && !builtInProvider) {
    return (
      <div className="flex flex-col gap-6">
        <h1 className="text-2xl font-bold">Provider Not Found</h1>
        <Card padding="lg">
          <p className="text-text-muted">The requested provider could not be found.</p>
        </Card>
      </div>
    );
  }
  if (isCustom && !customLoading && !customNode) {
    return (
      <div className="flex flex-col gap-6">
        <h1 className="text-2xl font-bold">Provider Not Found</h1>
        <Card padding="lg">
          <p className="text-text-muted">The requested provider could not be found.</p>
        </Card>
      </div>
    );
  }
  if (isCustom && customLoading) {
    return <div className="text-text-muted text-sm py-12 text-center">Loading...</div>;
  }

  const kinds = isCustom ? ["embedding"] : ((provider as any)?.serviceKinds ?? ["llm"]);
  if (!isCustom && !kinds.includes(kind)) {
    return (
      <div className="flex flex-col gap-6">
        <h1 className="text-2xl font-bold">Kind Mismatch</h1>
        <Card padding="lg">
          <p className="text-text-muted">This provider does not support the requested kind.</p>
        </Card>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-8">
      <div>
        <a
          href={`/dashboard/media-providers/${kind}`}
          className="inline-flex items-center gap-1 text-sm text-text-muted hover:text-brand-coral transition-colors mb-4"
        >
          <span className="material-symbols-outlined text-lg">arrow_back</span>
          {kindConfig.label}
        </a>

        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:gap-4">
          <div className="size-12 rounded-mini-lg flex items-center justify-center shrink-0" style={{ backgroundColor: `${(provider as any).color}15` }}>
            <ProviderIcon
              src={`/providers/${(provider as any).id}.png`}
              alt={(provider as any).name}
              size={48}
              className="object-contain rounded-mini-lg max-w-[48px] max-h-[48px]"
              fallbackText={(provider as any).textIcon || (provider as any).id.slice(0, 2).toUpperCase()}
              fallbackColor={(provider as any).color}
            />
          </div>
          <div className="flex-1">
            <div className="flex flex-wrap items-center gap-2 sm:gap-3">
              <h1 className="text-3xl font-semibold tracking-tight">{(provider as any).name}</h1>
              {!isCustom && (provider as any).notice?.apiKeyUrl && (
                <a
                  href={(provider as any).notice.apiKeyUrl}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="text-xs text-brand-coral hover:underline inline-flex items-center gap-1"
                >
                  <span className="material-symbols-outlined text-sm">open_in_new</span>
                  Get API Key
                </a>
              )}
            </div>
            <div className="flex items-center gap-1.5 mt-1 flex-wrap">
              {isCustom && <Badge variant="default" size="sm">Custom · {customNode?.prefix}</Badge>}
              {kinds.map((k: string) => (
                <Badge key={k} variant={k === kind ? "primary" : "default"} size="sm">
                  {k.toUpperCase()}
                </Badge>
              ))}
            </div>
          </div>
          {isCustom && (
            <div className="flex w-full flex-col gap-2 sm:w-auto sm:flex-row sm:items-center">
              <Button size="sm" variant="secondary" icon="edit" onClick={() => setShowEditModal(true)}>
                Edit
              </Button>
              <Button size="sm" variant="secondary" icon="delete" onClick={handleDeleteCustom}>
                Delete
              </Button>
            </div>
          )}
        </div>
      </div>

      {!isCustom && (provider as any)?.kindNotice?.[kind] && (
        <div className="flex items-start gap-3 px-4 py-3 rounded-mini-lg bg-amber-500/10 border border-amber-500/30 text-amber-700 dark:text-amber-400">
          <span className="material-symbols-outlined text-[20px] mt-0.5">warning</span>
          <p className="text-sm">{(provider as any).kindNotice[kind]}</p>
        </div>
      )}

      {!isCustom && (provider as any)?.notice?.text && !(provider as any)?.deprecated && (
        <div className="flex flex-col gap-2 rounded-mini-lg border border-blue-500/30 bg-blue-500/10 px-3 py-2 sm:flex-row sm:items-center">
          <span className="material-symbols-outlined text-[16px] text-blue-500 shrink-0">info</span>
          <p className="min-w-0 flex-1 text-xs leading-relaxed text-blue-600 dark:text-blue-400">{(provider as any).notice.text}</p>
          {(provider as any).notice.apiKeyUrl && (
            <a
              href={(provider as any).notice.apiKeyUrl}
              target="_blank"
              rel="noopener noreferrer"
              className="inline-flex justify-center rounded bg-blue-500 px-2 py-1 text-xs font-medium text-white transition-colors hover:bg-blue-600 sm:py-0.5"
            >
              Get API Key &rarr;
            </a>
          )}
        </div>
      )}

      {!isCustom && (provider as any)?.noAuth ? (
        <NoAuthProxyCard providerId={id} />
      ) : (
        <ConnectionsCard providerId={id} isOAuth={false} />
      )}

      {kind !== "tts" && kind !== "webSearch" && kind !== "webFetch" && (
        <ModelsCard
          providerId={id}
          kindFilter={kind}
          providerAliasOverride={isCustom ? customNode?.prefix : undefined}
        />
      )}

      {!isCustom && ((provider as any)?.searchConfig || (provider as any)?.fetchConfig || (provider as any)?.ttsConfig || (provider as any)?.sttConfig || (provider as any)?.embeddingConfig || (provider as any)?.searchViaChat) && (
        <ProviderInfoCard
          config={
            kind === "webFetch" ? (provider as any).fetchConfig
              : kind === "tts" ? (provider as any).ttsConfig
              : kind === "stt" ? (provider as any).sttConfig
              : kind === "embedding" ? (provider as any).embeddingConfig
              : (provider as any).searchConfig || { mode: "chat-completions", defaultModel: (provider as any).searchViaChat?.defaultModel, pricingUrl: (provider as any).searchViaChat?.pricingUrl, freeTier: (provider as any).searchViaChat?.freeTier }
          }
          provider={provider as any}
          title={`${kindConfig.label} Config`}
        />
      )}

      {kind === "embedding" && (
        <EmbeddingExampleCard providerId={id} customAlias={customNode?.prefix} />
      )}
      {kind === "tts" && <TtsExampleCard providerId={id} />}
      {kind === "stt" && !isCustom && <SttExampleCard providerId={id} />}
      {!isCustom && KIND_EXAMPLE_CONFIG[kind] && <GenericExampleCard providerId={id} kind={kind} />}

      {isCustom && (
        <AddCustomEmbeddingModal
          isOpen={showEditModal}
          node={customNode}
          onClose={() => setShowEditModal(false)}
          onSaved={(updated: any) => {
            setCustomNode(updated);
            setShowEditModal(false);
          }}
        />
      )}
    </div>
  );
}
