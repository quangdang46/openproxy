"use client";

import { useState, useEffect, useMemo } from "react";
import type { ChangeEvent } from "react";
import {
  Card,
  Button,
  ModelSelectModal,
  ManualConfigModal,
  McpMarketplaceModal,
} from "@/shared/components";
import type { McpMarketplaceAddPayload } from "@/shared/components/McpMarketplaceModal";
import {
  DEFAULT_PLUGINS,
  LOCAL_STDIO_PLUGINS,
} from "@/shared/constants/coworkPlugins";

const ENDPOINT = "/api/cli-tools/cowork-settings";

const isLocalhostUrl = (url: string): boolean =>
  /localhost|127\.0\.0\.1|0\.0\.0\.0/i.test(url || "");

const stripV1 = (url: string): string => (url || "").replace(/\/v1\/?$/, "");
const ensureV1 = (url: string): string => {
  const trimmed = (url || "").replace(/\/+$/, "");
  if (!trimmed) return "";
  return /\/v1$/.test(trimmed) ? trimmed : `${trimmed}/v1`;
};

interface Tool {
  name: string;
  description: string;
  image: string;
}

interface ApiKey {
  id: string;
  key: string;
}

interface CoworkPluginState {
  name: string;
  title?: string;
  description?: string;
  url?: string;
  transport?: string;
  oauth?: boolean;
  toolNames?: string[];
  custom?: boolean;
}

interface LocalStdioPluginDef {
  name: string;
  title?: string;
  description?: string;
  extensionUrl?: string;
  command?: string;
  args?: string[];
  toolNames?: string[];
}

interface CoworkStatus {
  installed: boolean;
  error?: string;
  hasOpenProxy?: boolean;
  defaultPlugins?: CoworkPluginState[];
  localStdioPlugins?: LocalStdioPluginDef[];
  cowork?: {
    baseUrl?: string;
    models?: string[];
    plugins?: CoworkPluginState[];
    localPlugins?: string[];
    customPlugins?: CoworkPluginState[];
  };
}

interface Message {
  type: "success" | "error";
  text: string;
}

interface CoworkToolCardProps {
  tool: Tool;
  isExpanded: boolean;
  onToggle: () => void;
  baseUrl: string;
  apiKeys: ApiKey[];
  activeProviders: string[];
  hasActiveProviders: boolean;
  cloudEnabled: boolean;
  cloudUrl?: string;
  tunnelEnabled: boolean;
  tunnelPublicUrl?: string;
  tailscaleEnabled: boolean;
  tailscaleUrl?: string;
  initialStatus?: CoworkStatus | null;
}

export default function CoworkToolCard({
  tool,
  isExpanded,
  onToggle,
  baseUrl,
  apiKeys,
  activeProviders,
  hasActiveProviders,
  cloudEnabled,
  cloudUrl,
  tunnelEnabled,
  tunnelPublicUrl,
  tailscaleEnabled,
  tailscaleUrl,
  initialStatus,
}: CoworkToolCardProps): React.ReactNode {
  const [status, setStatus] = useState<CoworkStatus | null>(initialStatus || null);
  const [checking, setChecking] = useState<boolean>(false);
  const [applying, setApplying] = useState<boolean>(false);
  const [restoring, setRestoring] = useState<boolean>(false);
  const [message, setMessage] = useState<Message | null>(null);
  const [selectedApiKey, setSelectedApiKey] = useState<string>("");
  const [selectedModels, setSelectedModels] = useState<string[]>([]);
  const [modalOpen, setModalOpen] = useState<boolean>(false);
  const [modelAliases, setModelAliases] = useState<Record<string, string>>({});
  const [showManualConfigModal, setShowManualConfigModal] = useState<boolean>(false);
  const [endpointMode, setEndpointMode] = useState<string>("custom");
  const [customBaseUrl, setCustomBaseUrl] = useState<string>("");
  const [plugins, setPlugins] = useState<CoworkPluginState[]>([]);
  const [localPlugins, setLocalPlugins] = useState<string[]>([]);
  const [customPlugins, setCustomPlugins] = useState<CoworkPluginState[]>([]);
  const [marketplaceOpen, setMarketplaceOpen] = useState<boolean>(false);
  const [addMcpOpen, setAddMcpOpen] = useState<boolean>(false);
  const [addMcpForm, setAddMcpForm] = useState<{ name: string; url: string }>({
    name: "",
    url: "",
  });

  const endpointOptions = useMemo(() => {
    const opts: Array<{ value: string; label: string; url: string }> = [];
    if (tunnelEnabled && tunnelPublicUrl) {
      opts.push({
        value: "tunnel",
        label: `Tunnel - ${tunnelPublicUrl}`,
        url: ensureV1(tunnelPublicUrl),
      });
    }
    if (tailscaleEnabled && tailscaleUrl) {
      opts.push({
        value: "tailscale",
        label: `Tailscale - ${tailscaleUrl}`,
        url: ensureV1(tailscaleUrl),
      });
    }
    if (cloudEnabled && cloudUrl) {
      opts.push({
        value: "cloud",
        label: `Cloud - ${cloudUrl}`,
        url: ensureV1(cloudUrl),
      });
    }
    opts.push({ value: "custom", label: "Custom URL (VPS / public host)", url: "" });
    return opts;
  }, [
    tunnelEnabled,
    tunnelPublicUrl,
    tailscaleEnabled,
    tailscaleUrl,
    cloudEnabled,
    cloudUrl,
  ]);

  const defaultPlugins = status?.defaultPlugins?.length
    ? status.defaultPlugins
    : DEFAULT_PLUGINS;
  const localStdioPlugins = status?.localStdioPlugins?.length
    ? status.localStdioPlugins
    : LOCAL_STDIO_PLUGINS;

  useEffect(() => {
    if (apiKeys?.length > 0 && !selectedApiKey) {
      setSelectedApiKey(apiKeys[0].key);
    }
  }, [apiKeys, selectedApiKey]);

  useEffect(() => {
    if (initialStatus) setStatus(initialStatus);
  }, [initialStatus]);

  useEffect(() => {
    if (isExpanded && !status) {
      checkStatus();
      fetchModelAliases();
    }
    if (isExpanded) fetchModelAliases();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isExpanded]);

  useEffect(() => {
    if (status?.cowork?.models?.length) {
      setSelectedModels(status.cowork.models);
    }
    if (status?.cowork?.baseUrl && !customBaseUrl) {
      setCustomBaseUrl(stripV1(status.cowork.baseUrl));
      setEndpointMode("custom");
    }
    // Initialize plugins: from current config, fallback to defaultPlugins
    if (Array.isArray(status?.cowork?.plugins) && status.cowork.plugins.length > 0) {
      setPlugins(status.cowork.plugins);
    } else if (plugins.length === 0 && Array.isArray(status?.defaultPlugins)) {
      setPlugins(status.defaultPlugins);
    } else if (plugins.length === 0 && status?.installed) {
      // Seed with frontend defaults when backend has not yet returned defaults
      setPlugins(DEFAULT_PLUGINS as CoworkPluginState[]);
    }
    if (Array.isArray(status?.cowork?.localPlugins)) {
      setLocalPlugins(status.cowork.localPlugins);
    }
    if (Array.isArray(status?.cowork?.customPlugins) && status.cowork.customPlugins.length > 0) {
      setCustomPlugins(status.cowork.customPlugins);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [status]);

  useEffect(() => {
    if (!customBaseUrl && endpointOptions[0]?.url) {
      setEndpointMode(endpointOptions[0].value);
      setCustomBaseUrl(stripV1(endpointOptions[0].url));
    }
  }, [endpointOptions, customBaseUrl]);

  const fetchModelAliases = async (): Promise<void> => {
    try {
      const res = await fetch("/api/models/alias");
      const data = await res.json();
      if (res.ok) setModelAliases(data.aliases || {});
    } catch (error) {
      console.log("Error fetching model aliases:", error);
    }
  };

  const checkStatus = async (): Promise<void> => {
    setChecking(true);
    try {
      const res = await fetch(ENDPOINT);
      const data = await res.json();
      setStatus(data);
    } catch (error) {
      setStatus({ installed: false, error: (error as Error).message });
    } finally {
      setChecking(false);
    }
  };

  const getEffectiveBaseUrl = (): string => ensureV1(customBaseUrl);

  const getConfigStatus = (): "configured" | "not_configured" | "invalid" | "other" | null => {
    if (!status?.installed) return null;
    const url = status?.cowork?.baseUrl;
    if (!url) return "not_configured";
    if (isLocalhostUrl(url)) return "invalid";
    return status.hasOpenProxy ? "configured" : "other";
  };

  const configStatus = getConfigStatus();
  const hasCustomSelectedApiKey =
    selectedApiKey && !apiKeys.some((key) => key.key === selectedApiKey);

  const handleEndpointModeChange = (value: string): void => {
    setEndpointMode(value);
    const opt = endpointOptions.find((o) => o.value === value);
    if (opt?.url) {
      setCustomBaseUrl(stripV1(opt.url));
    } else {
      setCustomBaseUrl("");
    }
  };

  const addPlugin = (p: McpMarketplaceAddPayload | CoworkPluginState): void => {
    if (!p?.name) return;
    if (plugins.some((x) => x.name === p.name)) return;
    setPlugins([...plugins, p as CoworkPluginState]);
  };

  const removePlugin = (name: string): void => {
    setPlugins(plugins.filter((p) => p.name !== name));
  };

  const handleApply = async (): Promise<void> => {
    setMessage(null);
    const effectiveUrl = getEffectiveBaseUrl();

    if (isLocalhostUrl(effectiveUrl)) {
      setMessage({
        type: "error",
        text: "Localhost is not allowed. Enable Tunnel/Tailscale or use VPS.",
      });
      return;
    }
    if (selectedModels.length === 0) {
      setMessage({ type: "error", text: "Please select at least one model" });
      return;
    }

    setApplying(true);
    try {
      const keyToUse =
        selectedApiKey?.trim() ||
        (apiKeys?.length > 0 ? apiKeys[0].key : null) ||
        (!cloudEnabled ? "sk_openproxy" : null);

      const res = await fetch(ENDPOINT, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          baseUrl: effectiveUrl,
          apiKey: keyToUse,
          models: selectedModels,
          plugins,
          localPlugins,
          customPlugins,
        }),
      });
      const data = await res.json();
      if (res.ok) {
        setMessage({
          type: "success",
          text: "Settings applied. Quit & reopen Claude Desktop to load.",
        });
        checkStatus();
      } else {
        setMessage({ type: "error", text: data.error || "Failed to apply settings" });
      }
    } catch (error) {
      setMessage({ type: "error", text: (error as Error).message });
    } finally {
      setApplying(false);
    }
  };

  const handleReset = async (): Promise<void> => {
    setRestoring(true);
    setMessage(null);
    try {
      const res = await fetch(ENDPOINT, { method: "DELETE" });
      const data = await res.json();
      if (res.ok) {
        setMessage({ type: "success", text: "Settings reset successfully" });
        setSelectedModels([]);
        setPlugins(defaultPlugins as CoworkPluginState[]);
        setLocalPlugins([]);
        setCustomPlugins([]);
        checkStatus();
      } else {
        setMessage({ type: "error", text: data.error || "Failed to reset" });
      }
    } catch (error) {
      setMessage({ type: "error", text: (error as Error).message });
    } finally {
      setRestoring(false);
    }
  };

  const getManualConfigs = (): Array<{ filename: string; content: string }> => {
    const keyToUse =
      selectedApiKey && selectedApiKey.trim()
        ? selectedApiKey
        : !cloudEnabled
          ? "sk_openproxy"
          : "<API_KEY_FROM_DASHBOARD>";

    const modelsToShow = selectedModels.length > 0 ? selectedModels : ["provider/model-id"];
    const cfg = {
      inferenceProvider: "gateway",
      inferenceGatewayBaseUrl: getEffectiveBaseUrl() || "https://your-public-host/v1",
      inferenceGatewayApiKey: keyToUse,
      inferenceModels: modelsToShow.map((name) => ({ name })),
    };

    return [
      {
        filename: "~/Library/Application Support/Claude-3p/configLibrary/<appliedId>.json",
        content: JSON.stringify(cfg, null, 2),
      },
    ];
  };

  return (
    <Card padding="xs" className="overflow-hidden">
      <div
        className="flex items-start justify-between gap-3 hover:cursor-pointer sm:items-center"
        onClick={onToggle}
      >
        <div className="flex min-w-0 items-center gap-3">
          <div className="size-8 flex items-center justify-center shrink-0">
            <img
              src={tool.image}
              alt={tool.name}
              width={32}
              height={32}
              className="size-8 object-contain rounded-lg"
              sizes="32px"
              onError={(e: React.SyntheticEvent<HTMLImageElement>) => {
                (e.target as HTMLImageElement).style.display = "none";
              }}
            />
          </div>
          <div className="min-w-0">
            <div className="flex min-w-0 flex-wrap items-center gap-2">
              <h3 className="font-medium text-sm">{tool.name}</h3>
              {configStatus === "configured" && (
                <span className="px-1.5 py-0.5 text-[10px] font-medium bg-green-500/10 text-green-600 dark:text-green-400 rounded-full">
                  Connected
                </span>
              )}
              {configStatus === "not_configured" && (
                <span className="px-1.5 py-0.5 text-[10px] font-medium bg-yellow-500/10 text-yellow-600 dark:text-yellow-400 rounded-full">
                  Not configured
                </span>
              )}
              {configStatus === "invalid" && (
                <span className="px-1.5 py-0.5 text-[10px] font-medium bg-red-500/10 text-red-600 dark:text-red-400 rounded-full">
                  Localhost (invalid)
                </span>
              )}
              {configStatus === "other" && (
                <span className="px-1.5 py-0.5 text-[10px] font-medium bg-blue-500/10 text-blue-600 dark:text-blue-400 rounded-full">
                  Other
                </span>
              )}
            </div>
            <p className="text-xs text-text-muted truncate">{tool.description}</p>
          </div>
        </div>
        <span
          className={`material-symbols-outlined text-text-muted text-[20px] transition-transform ${isExpanded ? "rotate-180" : ""}`}
        >
          expand_more
        </span>
      </div>

      {isExpanded && (
        <div className="mt-4 pt-4 border-t border-border flex flex-col gap-4">
          <div className="flex items-start gap-2 p-3 bg-blue-500/10 border border-blue-500/30 rounded-lg text-xs text-blue-700 dark:text-blue-300">
            <span className="material-symbols-outlined text-[16px] mt-0.5">info</span>
            <span>
              Claude Cowork runs in a sandboxed VM and <b>cannot reach localhost</b>. Use Tunnel,
              Tailscale, or VPS public URL.
            </span>
          </div>

          {checking && (
            <div className="flex items-center gap-2 text-text-muted">
              <span className="material-symbols-outlined animate-spin">progress_activity</span>
              <span>Checking Claude Cowork...</span>
            </div>
          )}

          {!checking && status && !status.installed && (
            <div className="flex flex-col gap-3 p-4 bg-yellow-500/10 border border-yellow-500/30 rounded-lg">
              <div className="flex items-start gap-3">
                <span className="material-symbols-outlined text-yellow-500">warning</span>
                <div className="flex-1">
                  <p className="font-medium text-yellow-600 dark:text-yellow-400">
                    Claude Desktop (Cowork mode) not detected
                  </p>
                  <p className="text-sm text-text-muted">
                    Open Claude Desktop → Help → Troubleshooting → Enable Developer mode → Configure
                    third-party inference, then return here.
                  </p>
                </div>
              </div>
              <div className="pl-9">
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() => setShowManualConfigModal(true)}
                  className="!bg-yellow-500/20 !border-yellow-500/40 !text-yellow-700 dark:!text-yellow-300 hover:!bg-yellow-500/30"
                >
                  <span className="material-symbols-outlined text-[18px] mr-1">content_copy</span>
                  Manual Config
                </Button>
              </div>
            </div>
          )}

          {!checking && status?.installed && (
            <>
              <div className="flex flex-col gap-2">
                {status?.cowork?.baseUrl && (
                  <div className="grid grid-cols-1 gap-1.5 sm:grid-cols-[8rem_auto_1fr_auto] sm:items-center sm:gap-2">
                    <span className="text-xs font-semibold text-text-main sm:text-right sm:text-sm">
                      Current
                    </span>
                    <span className="material-symbols-outlined hidden text-text-muted text-[14px] sm:inline">
                      arrow_forward
                    </span>
                    <span className="min-w-0 truncate rounded bg-surface/40 px-2 py-2 text-xs text-text-muted sm:py-1.5">
                      {status.cowork.baseUrl}
                    </span>
                  </div>
                )}

                <div className="grid grid-cols-1 gap-1.5 sm:grid-cols-[8rem_auto_1fr_auto] sm:items-center sm:gap-2">
                  <span className="text-xs font-semibold text-text-main sm:text-right sm:text-sm">
                    Endpoint Mode
                  </span>
                  <span className="material-symbols-outlined hidden text-text-muted text-[14px] sm:inline">
                    arrow_forward
                  </span>
                  <select
                    value={endpointMode}
                    onChange={(e: ChangeEvent<HTMLSelectElement>) =>
                      handleEndpointModeChange(e.target.value)
                    }
                    className="w-full min-w-0 px-2 py-2 bg-surface rounded text-xs border border-border focus:outline-none focus:ring-1 focus:ring-primary/50 sm:py-1.5"
                  >
                    {endpointOptions.map((opt) => (
                      <option key={opt.value} value={opt.value}>
                        {opt.label}
                      </option>
                    ))}
                  </select>
                </div>

                <div className="grid grid-cols-1 gap-1.5 sm:grid-cols-[8rem_auto_1fr_auto] sm:items-center sm:gap-2">
                  <span className="text-xs font-semibold text-text-main sm:text-right sm:text-sm">
                    Base URL
                  </span>
                  <span className="material-symbols-outlined hidden text-text-muted text-[14px] sm:inline">
                    arrow_forward
                  </span>
                  <input
                    type="text"
                    value={getEffectiveBaseUrl()}
                    onChange={(e: ChangeEvent<HTMLInputElement>) =>
                      setCustomBaseUrl(stripV1(e.target.value))
                    }
                    placeholder="https://your-host.com/v1"
                    className="w-full min-w-0 px-2 py-2 bg-surface rounded border border-border text-xs focus:outline-none focus:ring-1 focus:ring-primary/50 sm:py-1.5"
                  />
                </div>

                <div className="grid grid-cols-1 gap-1.5 sm:grid-cols-[8rem_auto_1fr_auto] sm:items-center sm:gap-2">
                  <span className="text-xs font-semibold text-text-main sm:text-right sm:text-sm">
                    API Key
                  </span>
                  <span className="material-symbols-outlined hidden text-text-muted text-[14px] sm:inline">
                    arrow_forward
                  </span>
                  {apiKeys.length > 0 || selectedApiKey ? (
                    <select
                      value={selectedApiKey}
                      onChange={(e: ChangeEvent<HTMLSelectElement>) =>
                        setSelectedApiKey(e.target.value)
                      }
                      className="w-full min-w-0 px-2 py-2 bg-surface rounded text-xs border border-border focus:outline-none focus:ring-1 focus:ring-primary/50 sm:py-1.5"
                    >
                      {hasCustomSelectedApiKey && (
                        <option value={selectedApiKey}>{selectedApiKey}</option>
                      )}
                      {apiKeys.map((key) => (
                        <option key={key.id} value={key.key}>
                          {key.key}
                        </option>
                      ))}
                    </select>
                  ) : (
                    <span className="min-w-0 rounded bg-surface/40 px-2 py-2 text-xs text-text-muted sm:py-1.5">
                      {cloudEnabled
                        ? "No API keys - Create one in Keys page"
                        : "sk_openproxy (default)"}
                    </span>
                  )}
                </div>

                <div className="grid grid-cols-1 gap-1.5 sm:grid-cols-[8rem_auto_1fr] sm:items-start sm:gap-2">
                  <span className="w-32 shrink-0 text-sm font-semibold text-text-main text-right pt-1">
                    Models
                  </span>
                  <span className="material-symbols-outlined text-text-muted text-[14px] mt-1.5">
                    arrow_forward
                  </span>
                  <div className="flex-1 flex flex-col gap-2">
                    <div className="flex flex-wrap gap-1.5 min-h-[28px] px-2 py-1.5 bg-surface rounded border border-border">
                      {selectedModels.length === 0 ? (
                        <span className="text-xs text-text-muted">No models selected</span>
                      ) : (
                        selectedModels.map((m) => (
                          <span
                            key={m}
                            className="inline-flex items-center gap-1 px-2 py-0.5 rounded text-xs bg-black/5 dark:bg-white/5 text-text-muted border border-transparent hover:border-border"
                          >
                            {m}
                            <button
                              type="button"
                              onClick={() =>
                                setSelectedModels((prev) => prev.filter((x) => x !== m))
                              }
                              className="ml-0.5 hover:text-red-500"
                            >
                              <span className="material-symbols-outlined text-[12px]">close</span>
                            </button>
                          </span>
                        ))
                      )}
                    </div>
                    <button
                      type="button"
                      onClick={() => setModalOpen(true)}
                      disabled={!hasActiveProviders}
                      className={`self-start px-2 py-1 rounded border text-xs transition-colors ${
                        hasActiveProviders
                          ? "bg-surface border-border text-text-main hover:border-primary cursor-pointer"
                          : "opacity-50 cursor-not-allowed border-border"
                      }`}
                    >
                      Add Model
                    </button>
                  </div>
                </div>

                {/* MCP plugins */}
                <div className="grid grid-cols-1 gap-1.5 sm:grid-cols-[8rem_auto_1fr] sm:items-start sm:gap-2">
                  <span className="w-32 shrink-0 text-sm font-semibold text-text-main text-right pt-2">
                    MCP
                  </span>
                  <span className="material-symbols-outlined text-text-muted text-[14px] mt-2">
                    arrow_forward
                  </span>
                  <div className="flex-1 flex flex-col gap-1">
                    {plugins
                      .filter((p) => p.name !== "exa")
                      .map((p) => (
                        <div
                          key={p.name}
                          className="flex items-center gap-2 px-2 py-1 bg-surface rounded border border-border"
                        >
                          <span className="text-xs font-medium min-w-0 truncate flex-shrink-0">
                            {p.title || p.name}
                          </span>
                          {p.oauth && (
                            <span className="text-[8px] text-amber-600 shrink-0">OAuth</span>
                          )}
                          <div
                            className="flex-1 flex flex-wrap gap-1 overflow-hidden"
                            style={{ maxHeight: "1.5rem" }}
                          >
                            {Array.isArray(p.toolNames) &&
                              p.toolNames.slice(0, 6).map((t) => (
                                <span
                                  key={t}
                                  className="text-[9px] px-1 py-0.5 rounded bg-black/5 dark:bg-white/5 text-text-muted whitespace-nowrap"
                                >
                                  {t}
                                </span>
                              ))}
                            {Array.isArray(p.toolNames) && p.toolNames.length > 6 && (
                              <span className="text-[9px] px-1 py-0.5 rounded bg-black/5 dark:bg-white/5 text-text-muted whitespace-nowrap">
                                +{p.toolNames.length - 6}
                              </span>
                            )}
                          </div>
                          <button
                            type="button"
                            onClick={() => removePlugin(p.name)}
                            className="shrink-0 hover:text-red-500 ml-auto"
                          >
                            <span className="material-symbols-outlined text-[12px]">close</span>
                          </button>
                        </div>
                      ))}
                    {customPlugins.map((p) => (
                      <div
                        key={p.name}
                        className="flex items-center gap-2 px-2 py-1 bg-surface rounded border border-border"
                      >
                        <span className="text-xs font-medium min-w-0 truncate flex-shrink-0">
                          {p.name}
                        </span>
                        <span className="text-[8px] px-1 py-0.5 rounded bg-blue-500/10 text-blue-500 shrink-0">
                          custom
                        </span>
                        <span className="flex-1 text-[9px] text-text-muted truncate">
                          {p.url}
                        </span>
                        <button
                          type="button"
                          onClick={() =>
                            setCustomPlugins(customPlugins.filter((x) => x.name !== p.name))
                          }
                          className="shrink-0 hover:text-red-500 ml-auto"
                        >
                          <span className="material-symbols-outlined text-[12px]">close</span>
                        </button>
                      </div>
                    ))}
                    {plugins.filter((p) => p.name !== "exa").length === 0 &&
                      customPlugins.length === 0 && (
                        <div className="px-2 py-1.5 bg-surface rounded border border-border text-xs text-text-muted">
                          No MCPs added
                        </div>
                      )}
                    <div className="flex items-center gap-2 mt-0.5">
                      <button
                        type="button"
                        onClick={() => setMarketplaceOpen(true)}
                        className="px-2 py-1 rounded border text-xs bg-primary/10 border-primary/40 text-primary hover:bg-primary/20 cursor-pointer whitespace-nowrap"
                      >
                        + Browse
                      </button>
                      <button
                        type="button"
                        onClick={() => {
                          setAddMcpForm({ name: "", url: "" });
                          setAddMcpOpen(true);
                        }}
                        className="px-2 py-1 rounded border text-xs bg-surface border-border text-text-muted hover:border-primary hover:text-primary cursor-pointer whitespace-nowrap"
                      >
                        + Custom
                      </button>
                      <a
                        href="https://mcp.so"
                        target="_blank"
                        rel="noopener noreferrer"
                        className="text-[10px] text-text-muted hover:text-primary underline ml-auto"
                      >
                        Find MCPs →
                      </a>
                    </div>
                  </div>
                </div>

                {/* Tools: Exa toggle + Browser MCP */}
                <div className="grid grid-cols-1 gap-1.5 sm:grid-cols-[8rem_auto_1fr] sm:items-start sm:gap-2">
                  <span className="w-32 shrink-0 text-sm font-semibold text-text-main text-right pt-1">
                    Tools
                  </span>
                  <span className="material-symbols-outlined text-text-muted text-[14px] mt-1.5">
                    arrow_forward
                  </span>
                  <div className="flex-1 flex flex-col gap-1.5">
                    {(() => {
                      const exaEnabled = plugins.some((p) => p.name === "exa");
                      const exaDef =
                        defaultPlugins.find((d) => d.name === "exa") ||
                        DEFAULT_PLUGINS.find((d) => d.name === "exa");
                      return (
                        <label className="flex items-start gap-2 cursor-pointer px-2 py-1.5 bg-surface rounded border border-border">
                          <input
                            type="checkbox"
                            checked={exaEnabled}
                            onChange={(e) => {
                              if (e.target.checked && exaDef) {
                                setPlugins([
                                  ...plugins.filter((p) => p.name !== "exa"),
                                  exaDef as CoworkPluginState,
                                ]);
                              } else {
                                setPlugins(plugins.filter((p) => p.name !== "exa"));
                              }
                            }}
                            className="mt-0.5"
                          />
                          <div className="flex-1 min-w-0">
                            <div className="text-xs font-medium">Web Search & Fetch (Exa)</div>
                            <p className="text-[10px] text-text-muted leading-snug">
                              Replaces built-in WebSearch/WebFetch. Auto-strips duplicates from tool
                              list.
                            </p>
                          </div>
                        </label>
                      );
                    })()}
                    {(() => {
                      const browserDef = localStdioPlugins.find((p) => p.name === "browsermcp");
                      if (!browserDef) return null;
                      const browserEnabled = localPlugins.includes("browsermcp");
                      return (
                        <label className="flex items-start gap-2 cursor-pointer px-2 py-1.5 bg-surface rounded border border-border">
                          <input
                            type="checkbox"
                            checked={browserEnabled}
                            onChange={(e) =>
                              setLocalPlugins(
                                e.target.checked
                                  ? [...localPlugins, "browsermcp"]
                                  : localPlugins.filter((n) => n !== "browsermcp"),
                              )
                            }
                            className="mt-0.5"
                          />
                          <div className="flex-1 min-w-0">
                            <div className="text-xs font-medium">
                              Browser Control (Browser MCP)
                            </div>
                            <p className="text-[10px] text-text-muted leading-snug">
                              Controls your running Chrome. Auto-strips Cowork&apos;s built-in
                              browser tools.{" "}
                              {browserDef.extensionUrl && (
                                <a
                                  href={browserDef.extensionUrl}
                                  target="_blank"
                                  rel="noopener noreferrer"
                                  className="text-primary underline"
                                >
                                  Install Chrome extension
                                </a>
                              )}
                            </p>
                          </div>
                        </label>
                      );
                    })()}
                  </div>
                </div>

                {/* Other local stdio plugins */}
                {localStdioPlugins.filter((p) => p.name !== "browsermcp").length > 0 && (
                  <div className="grid grid-cols-1 gap-1.5 sm:grid-cols-[8rem_auto_1fr] sm:items-start sm:gap-2">
                    <span className="w-32 shrink-0 text-sm font-semibold text-text-main text-right pt-1">
                      Local Plugins
                    </span>
                    <span className="material-symbols-outlined text-text-muted text-[14px] mt-1.5">
                      arrow_forward
                    </span>
                    <div className="flex-1 flex flex-col gap-2">
                      <div className="flex flex-col gap-1.5 px-2 py-1.5 bg-surface rounded border border-border">
                        {localStdioPlugins
                          .filter((p) => p.name !== "browsermcp")
                          .map((p) => {
                            const enabled = localPlugins.includes(p.name);
                            return (
                              <label
                                key={p.name}
                                className="flex items-start gap-2 cursor-pointer"
                              >
                                <input
                                  type="checkbox"
                                  checked={enabled}
                                  onChange={(e) =>
                                    setLocalPlugins(
                                      e.target.checked
                                        ? [...localPlugins, p.name]
                                        : localPlugins.filter((n) => n !== p.name),
                                    )
                                  }
                                  className="mt-0.5"
                                />
                                <div className="flex-1 min-w-0">
                                  <div className="flex flex-wrap items-center gap-1.5">
                                    <span className="text-xs font-medium">{p.title}</span>
                                    <span className="text-[8px] text-amber-600">stdio</span>
                                  </div>
                                  <p className="text-[10px] text-text-muted leading-snug">
                                    {p.description}
                                  </p>
                                  {p.extensionUrl && (
                                    <a
                                      href={p.extensionUrl}
                                      target="_blank"
                                      rel="noopener noreferrer"
                                      className="text-[10px] text-primary underline"
                                    >
                                      Install Chrome extension
                                    </a>
                                  )}
                                </div>
                              </label>
                            );
                          })}
                      </div>
                      <p className="text-[10px] text-text-muted leading-snug">
                        Local plugins run as subprocess via{" "}
                        <code className="px-1 py-0.5 rounded bg-black/5 dark:bg-white/5">npx</code>
                        . Requires Node.js installed.
                      </p>
                    </div>
                  </div>
                )}
              </div>

              {message && (
                <div
                  className={`flex items-center gap-2 px-2 py-1.5 rounded text-xs ${
                    message.type === "success"
                      ? "bg-green-500/10 text-green-600"
                      : "bg-red-500/10 text-red-600"
                  }`}
                >
                  <span className="material-symbols-outlined text-[14px]">
                    {message.type === "success" ? "check_circle" : "error"}
                  </span>
                  <span>{message.text}</span>
                </div>
              )}

              <div className="flex flex-col sm:flex-row sm:items-center gap-2">
                <Button
                  variant="primary"
                  size="sm"
                  onClick={handleApply}
                  disabled={selectedModels.length === 0}
                  loading={applying}
                  className="w-full sm:w-auto"
                >
                  <span className="material-symbols-outlined text-[14px] mr-1">save</span>Apply
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={handleReset}
                  disabled={!status.hasOpenProxy}
                  loading={restoring}
                  className="w-full sm:w-auto"
                >
                  <span className="material-symbols-outlined text-[14px] mr-1">restore</span>Reset
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => setShowManualConfigModal(true)}
                  className="w-full sm:w-auto"
                >
                  <span className="material-symbols-outlined text-[14px] mr-1">content_copy</span>
                  Manual Config
                </Button>
              </div>
            </>
          )}
        </div>
      )}

      <ModelSelectModal
        isOpen={modalOpen}
        onClose={() => setModalOpen(false)}
        onSelect={(model: { value: string }) => {
          if (!selectedModels.includes(model.value)) {
            setSelectedModels([...selectedModels, model.value]);
          }
          setModalOpen(false);
        }}
        selectedModel={null}
        activeProviders={activeProviders}
        modelAliases={modelAliases}
        title="Add Model for Claude Cowork"
      />

      <ManualConfigModal
        isOpen={showManualConfigModal}
        onClose={() => setShowManualConfigModal(false)}
        title="Claude Cowork - Manual Configuration"
        configs={getManualConfigs()}
      />

      <McpMarketplaceModal
        isOpen={marketplaceOpen}
        onClose={() => setMarketplaceOpen(false)}
        onAdd={addPlugin}
        addedNames={plugins.map((p) => p.name)}
      />

      {addMcpOpen && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/40"
          onClick={() => setAddMcpOpen(false)}
        >
          <div
            className="bg-surface border border-border rounded-xl shadow-xl w-full max-w-sm mx-4 p-5 flex flex-col gap-4"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex items-center justify-between">
              <h3 className="font-semibold text-sm">Add Custom MCP</h3>
              <button
                type="button"
                onClick={() => setAddMcpOpen(false)}
                className="text-text-muted hover:text-text-main"
              >
                <span className="material-symbols-outlined text-[18px]">close</span>
              </button>
            </div>

            <div className="flex flex-col gap-2">
              <div className="flex flex-col gap-1">
                <label className="text-[11px] text-text-muted font-medium">Name</label>
                <input
                  type="text"
                  placeholder="my-mcp"
                  value={addMcpForm.name}
                  onChange={(e) =>
                    setAddMcpForm((f) => ({
                      ...f,
                      name: e.target.value.replace(/\s+/g, "-").toLowerCase(),
                    }))
                  }
                  className="px-2 py-1.5 rounded border border-border bg-surface text-xs outline-none focus:border-primary"
                />
              </div>
              <div className="flex flex-col gap-1">
                <label className="text-[11px] text-text-muted font-medium">SSE URL</label>
                <input
                  type="text"
                  placeholder="https://your-mcp-server.com/sse"
                  value={addMcpForm.url}
                  onChange={(e) => setAddMcpForm((f) => ({ ...f, url: e.target.value }))}
                  className="px-2 py-1.5 rounded border border-border bg-surface text-xs outline-none focus:border-primary"
                />
              </div>
            </div>

            <div className="flex gap-2 justify-end">
              <button
                type="button"
                onClick={() => setAddMcpOpen(false)}
                className="px-3 py-1.5 rounded border border-border text-xs text-text-muted hover:bg-surface cursor-pointer"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={() => {
                  const name = addMcpForm.name.trim();
                  if (!name || !addMcpForm.url.trim()) return;
                  setCustomPlugins((prev) => [
                    ...prev.filter((x) => x.name !== name),
                    {
                      name,
                      url: addMcpForm.url.trim(),
                      transport: "sse",
                      custom: true,
                    },
                  ]);
                  setAddMcpOpen(false);
                }}
                className="px-3 py-1.5 rounded bg-primary text-white text-xs font-medium hover:opacity-90 cursor-pointer"
              >
                Add
              </button>
            </div>
          </div>
        </div>
      )}
    </Card>
  );
}
