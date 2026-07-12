"use client";

import { useState, useEffect, useCallback } from "react";
import { Card, CardSkeleton } from "@/shared/components";
import { CLI_TOOLS } from "@/shared/constants/cliTools";
import { getModelsByProviderId, PROVIDER_ID_TO_ALIAS, useEnsureCatalog } from "@/shared/constants/models";
import { ClaudeToolCard, ClineToolCard, KiloToolCard, CodexToolCard, DroidToolCard, OpenClawToolCard, HermesToolCard, DefaultToolCard, OpenCodeToolCard, CoworkToolCard, DeepSeekTuiToolCard, JcodeToolCard, MitmLinkCard } from "./cli-tools";
import { MITM_TOOLS } from "@/shared/constants/cliTools";

const CLOUD_URL: string | undefined = (import.meta.env as Record<string, string | undefined>)?.PUBLIC_CLOUD_URL;


const STATUS_ENDPOINTS: Record<string, string> = {
  claude: "/api/cli-tools/claude-settings",
  cline: "/api/cli-tools/cline-settings",
  kilo: "/api/cli-tools/kilo-settings",
  codex: "/api/cli-tools/codex-settings",
  opencode: "/api/cli-tools/opencode-settings",
  droid: "/api/cli-tools/droid-settings",
  openclaw: "/api/cli-tools/openclaw-settings",
  hermes: "/api/cli-tools/hermes-settings",
  cowork: "/api/cli-tools/cowork-settings",
  "deepseek-tui": "/api/cli-tools/deepseek-tui-settings",
  jcode: "/api/cli-tools/jcode-settings",
};

interface CLIToolsPageClientProps {
  machineId?: string;
}

export default function CLIToolsPageClient({ machineId }: CLIToolsPageClientProps) {
  useEnsureCatalog();
  const [connections, setConnections] = useState<any[]>([]);
  const [loading, setLoading] = useState<boolean>(true);
  const [expandedTool, setExpandedTool] = useState<string | null>(null);
  const [modelMappings, setModelMappings] = useState<Record<string, Record<string, string>>>({});
  const [cloudEnabled, setCloudEnabled] = useState<boolean>(false);
  const [tunnelEnabled, setTunnelEnabled] = useState<boolean>(false);
  const [tunnelPublicUrl, setTunnelPublicUrl] = useState<string>("");
  const [tailscaleEnabled, setTailscaleEnabled] = useState<boolean>(false);
  const [tailscaleUrl, setTailscaleUrl] = useState<string>("");
  const [apiKeys, setApiKeys] = useState<any[]>([]);
  const [toolStatuses, setToolStatuses] = useState<Record<string, any>>({});

  useEffect(() => {
    fetchConnections();
    loadCloudSettings();
    fetchApiKeys();
    fetchAllStatuses();
  }, []);

  const fetchAllStatuses = async () => {
    try {
      // Prefer the aggregation endpoint; fall back to per-tool parallel fetches.
      const aggRes = await fetch("/api/cli-tools/all-statuses").catch(() => null);
      if (aggRes && aggRes.ok) {
        const data = await aggRes.json();
        if (data && typeof data === "object" && !("error" in data)) {
          setToolStatuses(data);
          return;
        }
      }
    } catch {
      // fall through to per-tool
    }

    // Per-tool fallback
    try {
      const entries = await Promise.all(
        Object.entries(STATUS_ENDPOINTS).map(async ([toolId, url]) => {
          try {
            const res = await fetch(url);
            const data = await res.json();
            return [toolId, data];
          } catch {
            return [toolId, null];
          }
        })
      );
      setToolStatuses(Object.fromEntries(entries));
    } catch (error) {
      console.log("Error fetching tool statuses:", error);
    }
  };

  const loadCloudSettings = async () => {
    try {
      const [settingsRes, tunnelRes] = await Promise.all([
        fetch("/api/settings"),
        fetch("/api/tunnel/status"),
      ]);
      if (settingsRes.ok) {
        const data = await settingsRes.json();
        setCloudEnabled(data.cloudEnabled || false);
      }
      if (tunnelRes.ok) {
        const data = await tunnelRes.json();
        setTunnelEnabled(!!(data.tunnel?.enabled || data.tunnel?.settingsEnabled));
        setTunnelPublicUrl(data.tunnel?.publicUrl || "");
        setTailscaleEnabled(!!(data.tailscale?.enabled || data.tailscale?.settingsEnabled));
        setTailscaleUrl(data.tailscale?.tunnelUrl || "");
      }
    } catch (error) {
      console.log("Error loading settings:", error);
    }
  };

  const fetchApiKeys = async () => {
    try {
      const res = await fetch("/api/keys");
      if (res.ok) {
        const data = await res.json();
        setApiKeys(data.keys || []);
      }
    } catch (error) {
      console.log("Error fetching API keys:", error);
    }
  };

  const fetchConnections = async () => {
    try {
      const res = await fetch("/api/providers");
      const data = await res.json();
      if (res.ok) {
        setConnections(data.connections || []);
      }
    } catch (error) {
      console.log("Error fetching connections:", error);
    } finally {
      setLoading(false);
    }
  };

  const getActiveProviders = () => connections.filter((c: any) => c.isActive !== false);

  const getAllAvailableModels = () => {
    const activeProviders = getActiveProviders();
    const models: any[] = [];
    const seenModels = new Set<string>();
    activeProviders.forEach((conn: any) => {
      const alias = PROVIDER_ID_TO_ALIAS[conn.provider] || conn.provider;
      const providerModels = getModelsByProviderId(conn.provider);
      providerModels.forEach((m: any) => {
        const modelValue = `${alias}/${m.id}`;
        if (!seenModels.has(modelValue)) {
          seenModels.add(modelValue);
          models.push({ value: modelValue, label: `${alias}/${m.id}`, provider: conn.provider, alias, connectionName: conn.name, modelId: m.id });
        }
      });
    });
    return models;
  };

  const handleModelMappingChange = useCallback((toolId: string, modelAlias: string, targetModel: string) => {
    setModelMappings(prev => {
      if (prev[toolId]?.[modelAlias] === targetModel) return prev;
      return { ...prev, [toolId]: { ...prev[toolId], [modelAlias]: targetModel } };
    });
  }, []);

  const getBaseUrl = (): string => {
    if (tunnelEnabled && tunnelPublicUrl) return tunnelPublicUrl;
    if (cloudEnabled && CLOUD_URL) return CLOUD_URL;
    if (typeof window !== "undefined") return window.location.origin;
    return "http://localhost:4623";
  };

  if (loading) {
    return (
      <div className="flex flex-col gap-4">
        <CardSkeleton />
        <CardSkeleton />
        <CardSkeleton />
      </div>
    );
  }

  const availableModels = getAllAvailableModels();
  const hasActiveProviders = availableModels.length > 0;

  const renderToolCard = (toolId: string, tool: any) => {
    const commonProps = {
      tool,
      isExpanded: expandedTool === toolId,
      onToggle: () => setExpandedTool(expandedTool === toolId ? null : toolId),
      baseUrl: getBaseUrl(),
      apiKeys,
    };

    switch (toolId) {
      case "claude":
        return (
          <ClaudeToolCard
            key={toolId}
            {...commonProps}
            activeProviders={getActiveProviders()}
            modelMappings={modelMappings[toolId] || {}}
            onModelMappingChange={(alias, target) => handleModelMappingChange(toolId, alias, target)}
            hasActiveProviders={hasActiveProviders}
            cloudEnabled={cloudEnabled}
            initialStatus={toolStatuses.claude}
          />
        );
      case "cline":
        return <ClineToolCard key={toolId} {...commonProps} activeProviders={getActiveProviders()} cloudEnabled={cloudEnabled} initialStatus={toolStatuses.cline} />;
      case "kilo":
        return <KiloToolCard key={toolId} {...commonProps} activeProviders={getActiveProviders()} cloudEnabled={cloudEnabled} initialStatus={toolStatuses.kilo} />;
      case "codex":
        return <CodexToolCard key={toolId} {...commonProps} activeProviders={getActiveProviders()} cloudEnabled={cloudEnabled} initialStatus={toolStatuses.codex} />;
      case "opencode":
        return <OpenCodeToolCard key={toolId} {...commonProps} activeProviders={getActiveProviders()} cloudEnabled={cloudEnabled} initialStatus={toolStatuses.opencode} />;
      case "cowork":
        return (
          <CoworkToolCard
            key={toolId}
            {...commonProps}
            activeProviders={getActiveProviders()}
            hasActiveProviders={hasActiveProviders}
            cloudEnabled={cloudEnabled}
            cloudUrl={CLOUD_URL}
            tunnelEnabled={tunnelEnabled}
            tunnelPublicUrl={tunnelPublicUrl}
            tailscaleEnabled={tailscaleEnabled}
            tailscaleUrl={tailscaleUrl}
            initialStatus={toolStatuses.cowork}
          />
        );
      case "droid":
        return <DroidToolCard key={toolId} {...commonProps} activeProviders={getActiveProviders()} hasActiveProviders={hasActiveProviders} cloudEnabled={cloudEnabled} initialStatus={toolStatuses.droid} />;
      case "openclaw":
        return <OpenClawToolCard key={toolId} {...commonProps} activeProviders={getActiveProviders()} hasActiveProviders={hasActiveProviders} cloudEnabled={cloudEnabled} cloudUrl={CLOUD_URL} tunnelEnabled={tunnelEnabled} tunnelPublicUrl={tunnelPublicUrl} tailscaleEnabled={tailscaleEnabled} tailscaleUrl={tailscaleUrl} initialStatus={toolStatuses.openclaw} />;
      case "hermes":
        return <HermesToolCard key={toolId} {...commonProps} activeProviders={getActiveProviders()} hasActiveProviders={hasActiveProviders} cloudEnabled={cloudEnabled} initialStatus={toolStatuses.hermes} />;
      case "deepseek-tui":
        return <DeepSeekTuiToolCard key={toolId} {...commonProps} activeProviders={getActiveProviders()} hasActiveProviders={hasActiveProviders} cloudEnabled={cloudEnabled} initialStatus={toolStatuses["deepseek-tui"]} />;
      case "jcode":
        return <JcodeToolCard key={toolId} {...commonProps} activeProviders={getActiveProviders()} hasActiveProviders={hasActiveProviders} cloudEnabled={cloudEnabled} cloudUrl={CLOUD_URL} tunnelEnabled={tunnelEnabled} tunnelPublicUrl={tunnelPublicUrl} tailscaleEnabled={tailscaleEnabled} tailscaleUrl={tailscaleUrl} initialStatus={toolStatuses.jcode} />;
      default:
        return <DefaultToolCard key={toolId} toolId={toolId} {...commonProps} activeProviders={getActiveProviders()} cloudEnabled={cloudEnabled} tunnelEnabled={tunnelEnabled} />;
    }
  };

  const regularTools = Object.entries(CLI_TOOLS);
  const mitmTools = Object.entries(MITM_TOOLS);

  return (
    <div className="mx-auto flex w-full max-w-5xl flex-col gap-6 px-1 sm:px-0">
      <div className="flex flex-col gap-1">
        <h1 className="text-xl font-semibold text-text-main sm:text-2xl">CLI Tools</h1>
        <p className="text-sm text-text-muted">Configure local coding tools to use your OpenProxy providers.</p>
      </div>
      <div className="grid gap-3 sm:gap-4">
        {regularTools.map(([toolId, tool]) => renderToolCard(toolId, tool))}
      </div>
      <div className="grid gap-3 sm:gap-4">
        <div className="flex items-center gap-2 px-1">
          <span className="material-symbols-outlined text-[18px] text-primary">security</span>
          <h2 className="text-sm font-semibold text-text-main">MITM Tools</h2>
        </div>
        {mitmTools.map(([toolId, tool]) => (
          <MitmLinkCard key={toolId} tool={tool} />
        ))}
      </div>
    </div>
  );
}
