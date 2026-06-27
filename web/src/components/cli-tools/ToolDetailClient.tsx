"use client";

import { useState, useEffect, useCallback } from "react";
import { CardSkeleton } from "@/shared/components";
import { CLI_TOOLS, MITM_TOOLS } from "@/shared/constants/cliTools";
import { getModelsByProviderId, PROVIDER_ID_TO_ALIAS, useEnsureCatalog } from "@/shared/constants/models";
import {
  ClaudeToolCard,
  CodexToolCard,
  DroidToolCard,
  OpenClawToolCard,
  HermesToolCard,
  DefaultToolCard,
  OpenCodeToolCard,
  CoworkToolCard,
  CopilotToolCard,
  ClineToolCard,
  KiloToolCard,
  DeepSeekTuiToolCard,
  JcodeToolCard,
  MitmLinkCard,
} from "@/components/cli-tools";

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

export default function ToolDetailClient() {
  useEnsureCatalog();
  const [toolId, setToolId] = useState<string>("");
  const [mounted, setMounted] = useState<boolean>(false);
  const [connections, setConnections] = useState<any[]>([]);
  const [loading, setLoading] = useState<boolean>(true);
  const [modelMappings, setModelMappings] = useState<Record<string, Record<string, string>>>({});
  const [cloudEnabled, setCloudEnabled] = useState<boolean>(false);
  const [tunnelEnabled, setTunnelEnabled] = useState<boolean>(false);
  const [tunnelPublicUrl, setTunnelPublicUrl] = useState<string>("");
  const [tailscaleEnabled, setTailscaleEnabled] = useState<boolean>(false);
  const [tailscaleUrl, setTailscaleUrl] = useState<string>("");
  const [apiKeys, setApiKeys] = useState<any[]>([]);
  const [toolStatus, setToolStatus] = useState<any>(null);

  useEffect(() => {
    setMounted(true);
    // Parse toolId from /dashboard/cli-tools/<toolId>
    const pathParts = window.location.pathname.split("/").filter(Boolean);
    // pathParts = ["dashboard", "cli-tools", "<toolId>"]
    const id = pathParts.length >= 3 ? pathParts[pathParts.length - 1] : "";
    setToolId(id);
  }, []);

  const tool = CLI_TOOLS[toolId] || MITM_TOOLS[toolId];
  const isMitm = !!MITM_TOOLS[toolId];

  useEffect(() => {
    if (!toolId) return;
    let mountedFlag = true;
    (async () => {
      try {
        const [provRes, settingsRes, tunnelRes, keysRes] = await Promise.all([
          fetch("/api/providers"),
          fetch("/api/settings"),
          fetch("/api/tunnel/status"),
          fetch("/api/keys"),
        ]);
        if (!mountedFlag) return;
        if (provRes.ok) {
          const data = await provRes.json();
          setConnections(data.connections || []);
        }
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
        if (keysRes.ok) {
          const data = await keysRes.json();
          setApiKeys(data.keys || []);
        }
        // Fetch tool-specific status
        const statusEndpoint = STATUS_ENDPOINTS[toolId];
        if (statusEndpoint) {
          try {
            const statusRes = await fetch(statusEndpoint);
            if (statusRes.ok) {
              const statusData = await statusRes.json();
              if (mountedFlag) setToolStatus(statusData);
            }
          } catch {
            // Status fetch is non-critical
          }
        }
      } catch (error) {
        console.log("Error loading tool data:", error);
      } finally {
        if (mountedFlag) setLoading(false);
      }
    })();
    return () => { mountedFlag = false; };
  }, [toolId]);

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

  const handleModelMappingChange = useCallback((tId: string, alias: string, target: string) => {
    setModelMappings((prev) => {
      if (prev[tId]?.[alias] === target) return prev;
      return { ...prev, [tId]: { ...prev[tId], [alias]: target } };
    });
  }, []);

  const getBaseUrl = (): string => {
    if (tunnelEnabled && tunnelPublicUrl) return tunnelPublicUrl;
    if (cloudEnabled && CLOUD_URL) return CLOUD_URL;
    if (typeof window !== "undefined") return window.location.origin;
    return "http://localhost:4623";
  };

  const renderToolCard = () => {
    const availableModels = getAllAvailableModels();
    const hasActiveProviders = availableModels.length > 0;
    const commonProps = {
      tool,
      isExpanded: true,
      onToggle: () => {},
      baseUrl: getBaseUrl(),
      apiKeys,
      tunnelEnabled,
      tunnelPublicUrl,
      tailscaleEnabled,
      tailscaleUrl,
      cloudUrl: CLOUD_URL,
    };

    // MITM tools render as a MitmLinkCard
    if (isMitm) {
      return <MitmLinkCard tool={tool} />;
    }

    switch (toolId) {
      case "claude":
        return (
          <ClaudeToolCard
            {...commonProps}
            activeProviders={getActiveProviders()}
            modelMappings={modelMappings[toolId] || {}}
            onModelMappingChange={(a: string, t: string) => handleModelMappingChange(toolId, a, t)}
            hasActiveProviders={hasActiveProviders}
            cloudEnabled={cloudEnabled}
            initialStatus={toolStatus}
          />
        );
      case "codex":
        return <CodexToolCard {...commonProps} activeProviders={getActiveProviders()} cloudEnabled={cloudEnabled} initialStatus={toolStatus} />;
      case "opencode":
        return <OpenCodeToolCard {...commonProps} activeProviders={getActiveProviders()} cloudEnabled={cloudEnabled} initialStatus={toolStatus} />;
      case "cowork":
        return (
          <CoworkToolCard
            {...commonProps}
            activeProviders={getActiveProviders()}
            hasActiveProviders={hasActiveProviders}
            cloudEnabled={cloudEnabled}
            cloudUrl={CLOUD_URL}
            tunnelEnabled={tunnelEnabled}
            tunnelPublicUrl={tunnelPublicUrl}
            tailscaleEnabled={tailscaleEnabled}
            tailscaleUrl={tailscaleUrl}
            initialStatus={toolStatus}
          />
        );
      case "droid":
        return <DroidToolCard {...commonProps} activeProviders={getActiveProviders()} hasActiveProviders={hasActiveProviders} cloudEnabled={cloudEnabled} initialStatus={toolStatus} />;
      case "openclaw":
        return <OpenClawToolCard {...commonProps} activeProviders={getActiveProviders()} hasActiveProviders={hasActiveProviders} cloudEnabled={cloudEnabled} initialStatus={toolStatus} />;
      case "hermes":
        return <HermesToolCard {...commonProps} activeProviders={getActiveProviders()} hasActiveProviders={hasActiveProviders} cloudEnabled={cloudEnabled} initialStatus={toolStatus} />;
      case "copilot":
        return <CopilotToolCard {...commonProps} activeProviders={getActiveProviders()} cloudEnabled={cloudEnabled} />;
      case "cline":
        return <ClineToolCard {...commonProps} activeProviders={getActiveProviders()} cloudEnabled={cloudEnabled} initialStatus={toolStatus} />;
      case "kilo":
        return <KiloToolCard {...commonProps} activeProviders={getActiveProviders()} cloudEnabled={cloudEnabled} initialStatus={toolStatus} />;
      case "deepseek-tui":
        return <DeepSeekTuiToolCard {...commonProps} activeProviders={getActiveProviders()} hasActiveProviders={hasActiveProviders} cloudEnabled={cloudEnabled} initialStatus={toolStatus} />;
      case "jcode":
        return <JcodeToolCard {...commonProps} activeProviders={getActiveProviders()} hasActiveProviders={hasActiveProviders} cloudEnabled={cloudEnabled} initialStatus={toolStatus} />;
      default:
        return <DefaultToolCard toolId={toolId} {...commonProps} activeProviders={getActiveProviders()} cloudEnabled={cloudEnabled} tunnelEnabled={tunnelEnabled} />;
    }
  };

  if (!toolId) return null;

  // Guard removed/unknown tools to avoid crash on direct URL
  if (!tool) {
    return (
      <div className="mx-auto flex w-full max-w-5xl flex-col gap-4 px-1 sm:px-0">
        <a href="/dashboard/cli-tools" className="inline-flex items-center gap-1 text-sm text-text-muted hover:text-primary w-fit">
          <span className="material-symbols-outlined text-[18px]">arrow_back</span>
          Back to CLI Tools
        </a>
        <p className="text-sm text-text-muted">Tool not found or disabled.</p>
      </div>
    );
  }

  return (
    <div className="mx-auto flex w-full max-w-5xl flex-col gap-4 px-1 sm:px-0">
      <a href="/dashboard/cli-tools" className="inline-flex items-center gap-1 text-sm text-text-muted hover:text-primary w-fit">
        <span className="material-symbols-outlined text-[18px]">arrow_back</span>
        Back to CLI Tools
      </a>
      <div className="flex flex-col gap-1">
        <h1 className="text-xl font-semibold text-text-main sm:text-2xl">{tool.name}</h1>
        <p className="text-sm text-text-muted">{tool.description}</p>
      </div>
      {loading ? <CardSkeleton /> : renderToolCard()}
    </div>
  );
}
