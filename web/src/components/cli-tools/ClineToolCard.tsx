"use client";

import { useState, useEffect } from "react";
import type { ChangeEvent } from "react";
import { Card, Button, ModelSelectModal, ManualConfigModal } from "@/shared/components";
import EndpointPresetControl from "./EndpointPresetControl";

interface Tool {
  name: string;
  description: string;
}

interface ApiKey {
  id: string;
  key: string;
}

interface ClineStatus {
  installed: boolean;
  error?: string;
  hasOpenProxy?: boolean;
  settings?: {
    actModeApiProvider?: string;
    planModeApiProvider?: string;
    openAiBaseUrl?: string;
    openAiModelId?: string;
  };
  globalStatePath?: string;
}

interface Message {
  type: "success" | "error";
  text: string;
}

interface ClineToolCardProps {
  tool: Tool;
  isExpanded: boolean;
  onToggle: () => void;
  baseUrl: string;
  apiKeys: ApiKey[];
  activeProviders: string[];
  cloudEnabled: boolean;
  initialStatus?: ClineStatus | null;
}

export default function ClineToolCard({
  tool,
  isExpanded,
  onToggle,
  baseUrl,
  apiKeys,
  activeProviders,
  cloudEnabled,
  initialStatus,
}: ClineToolCardProps): React.ReactNode {
  const [status, setStatus] = useState<ClineStatus | null>(initialStatus || null);
  const [checking, setChecking] = useState<boolean>(false);
  const [applying, setApplying] = useState<boolean>(false);
  const [restoring, setRestoring] = useState<boolean>(false);
  const [message, setMessage] = useState<Message | null>(null);
  const [selectedApiKey, setSelectedApiKey] = useState<string>("");
  const [selectedModel, setSelectedModel] = useState<string>("");
  const [modalOpen, setModalOpen] = useState<boolean>(false);
  const [showManualConfigModal, setShowManualConfigModal] = useState<boolean>(false);
  const [customBaseUrl, setCustomBaseUrl] = useState<string>("");

  const normalizeLocalhost = (url: string): string => url.replace("://localhost", "://127.0.0.1");

  const getLocalBaseUrl = (): string => {
    if (typeof window !== "undefined") {
      return normalizeLocalhost(window.location.origin);
    }
    return "http://127.0.0.1:4623";
  };

  // Cline expects base WITHOUT /v1, but we render WITH /v1 for consistency with
  // other tools. The backend strips /v1 before persisting.
  const getDisplayUrl = (): string => {
    const url = customBaseUrl || getLocalBaseUrl();
    return url.endsWith("/v1") ? url : `${url}/v1`;
  };

  const getEffectiveBaseUrl = (): string => getDisplayUrl();

  const hasCustomSelectedApiKey =
    selectedApiKey && !apiKeys.some((key) => key.key === selectedApiKey);

  const getConfigStatus = (): "configured" | "not_configured" | "other" | null => {
    if (!status?.installed) return null;
    if (!status.hasOpenProxy) return "not_configured";
    const url = status.settings?.openAiBaseUrl || "";
    const localMatch = url.includes("localhost") || url.includes("127.0.0.1") || url.includes("0.0.0.0");
    return localMatch ? "configured" : "other";
  };

  const configStatus = getConfigStatus();

  useEffect(() => {
    if (apiKeys?.length > 0 && !selectedApiKey) setSelectedApiKey(apiKeys[0].key);
  }, [apiKeys, selectedApiKey]);

  useEffect(() => {
    if (initialStatus) setStatus(initialStatus);
  }, [initialStatus]);

  useEffect(() => {
    if (isExpanded && !status) {
      void checkStatus();
    }
  }, [isExpanded]);

  useEffect(() => {
    if (status?.settings?.openAiModelId) setSelectedModel(status.settings.openAiModelId);
  }, [status]);

  const checkStatus = async (): Promise<void> => {
    setChecking(true);
    try {
      const res = await fetch("/api/cli-tools/cline-settings");
      const data = await res.json();
      setStatus(data);
    } catch (error) {
      setStatus({ installed: false, error: (error as Error).message });
    } finally {
      setChecking(false);
    }
  };

  const handleApply = async (): Promise<void> => {
    setApplying(true);
    setMessage(null);
    try {
      const keyToUse =
        selectedApiKey?.trim() ||
        (apiKeys?.length > 0 ? apiKeys[0].key : null) ||
        (!cloudEnabled ? "sk_openproxy" : null);

      const res = await fetch("/api/cli-tools/cline-settings", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          baseUrl: getEffectiveBaseUrl(),
          apiKey: keyToUse,
          model: selectedModel,
        }),
      });
      const data = await res.json();
      if (res.ok) {
        setMessage({ type: "success", text: "Settings applied successfully!" });
        void checkStatus();
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
      const res = await fetch("/api/cli-tools/cline-settings", { method: "DELETE" });
      const data = await res.json();
      if (res.ok) {
        setMessage({ type: "success", text: "Settings reset successfully!" });
        setSelectedModel("");
        void checkStatus();
      } else {
        setMessage({ type: "error", text: data.error || "Failed to reset settings" });
      }
    } catch (error) {
      setMessage({ type: "error", text: (error as Error).message });
    } finally {
      setRestoring(false);
    }
  };

  const handleModelSelect = (model: { value: string }): void => {
    setSelectedModel(model.value);
    setModalOpen(false);
  };

  const getManualConfigs = (): Array<{ filename: string; content: string }> => {
    const keyToUse =
      selectedApiKey?.trim() ||
      (apiKeys?.length > 0 ? apiKeys[0].key : null) ||
      (!cloudEnabled ? "sk_openproxy" : "<API_KEY_FROM_DASHBOARD>");
    const effectiveUrl = getEffectiveBaseUrl();
    const baseWithoutV1 = effectiveUrl.endsWith("/v1") ? effectiveUrl.slice(0, -3) : effectiveUrl;
    return [
      {
        filename: "~/.cline/data/globalState.json",
        content: JSON.stringify(
          {
            actModeApiProvider: "openai",
            planModeApiProvider: "openai",
            openAiBaseUrl: baseWithoutV1,
            openAiModelId: selectedModel || "provider/model-id",
            planModeOpenAiModelId: selectedModel || "provider/model-id",
          },
          null,
          2,
        ),
      },
      {
        filename: "~/.cline/data/secrets.json",
        content: JSON.stringify({ openAiApiKey: keyToUse }, null, 2),
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
              src="/providers/cline.png"
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
          {checking && (
            <div className="flex items-center gap-2 text-text-muted">
              <span className="material-symbols-outlined animate-spin">progress_activity</span>
              <span>Checking Cline CLI...</span>
            </div>
          )}

          {!checking && status && !status.installed && (
            <div className="flex flex-col gap-4">
              <div className="flex flex-col gap-3 p-4 bg-yellow-500/10 border border-yellow-500/30 rounded-lg">
                <div className="flex items-start gap-3">
                  <span className="material-symbols-outlined text-yellow-500">warning</span>
                  <div className="flex-1">
                    <p className="font-medium text-yellow-600 dark:text-yellow-400">
                      Cline CLI not detected locally
                    </p>
                    <p className="text-sm text-text-muted">
                      Install Cline from{" "}
                      <a
                        className="text-primary underline"
                        href="https://docs.cline.bot/"
                        target="_blank"
                        rel="noreferrer"
                      >
                        docs.cline.bot
                      </a>{" "}
                      or use Manual Config below.
                    </p>
                  </div>
                </div>
                <div className="flex items-center gap-2 pl-9">
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
            </div>
          )}

          {!checking && status?.installed && (
            <>
              <div className="flex flex-col gap-2">
                {status?.settings?.openAiBaseUrl && (
                  <div className="grid grid-cols-1 gap-1.5 sm:grid-cols-[8rem_auto_1fr_auto] sm:items-center sm:gap-2">
                    <span className="text-xs font-semibold text-text-main sm:text-right sm:text-sm">
                      Current
                    </span>
                    <span className="material-symbols-outlined hidden text-text-muted text-[14px] sm:inline">
                      arrow_forward
                    </span>
                    <span className="min-w-0 truncate rounded bg-surface/40 px-2 py-2 text-xs text-text-muted sm:py-1.5">
                      {status.settings.openAiBaseUrl}
                    </span>
                  </div>
                )}

                <EndpointPresetControl
                  baseUrl={getDisplayUrl()}
                  apiKey={selectedApiKey}
                  onBaseUrlChange={setCustomBaseUrl}
                  onApiKeyChange={setSelectedApiKey}
                />

                <div className="grid grid-cols-1 gap-1.5 sm:grid-cols-[8rem_auto_1fr_auto] sm:items-center sm:gap-2">
                  <span className="text-xs font-semibold text-text-main sm:text-right sm:text-sm">
                    Base URL
                  </span>
                  <span className="material-symbols-outlined hidden text-text-muted text-[14px] sm:inline">
                    arrow_forward
                  </span>
                  <input
                    type="text"
                    value={getDisplayUrl()}
                    onChange={(e: ChangeEvent<HTMLInputElement>) => setCustomBaseUrl(e.target.value)}
                    placeholder="https://.../v1"
                    className="w-full min-w-0 px-2 py-2 bg-surface rounded border border-border text-xs focus:outline-none focus:ring-1 focus:ring-primary/50 sm:py-1.5"
                  />
                  {customBaseUrl && customBaseUrl !== baseUrl && (
                    <button
                      onClick={() => setCustomBaseUrl("")}
                      className="p-1 text-text-muted hover:text-primary rounded transition-colors"
                      title="Reset to default"
                    >
                      <span className="material-symbols-outlined text-[14px]">restart_alt</span>
                    </button>
                  )}
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
                      onChange={(e: ChangeEvent<HTMLSelectElement>) => setSelectedApiKey(e.target.value)}
                      className="w-full min-w-0 px-2 py-2 bg-surface rounded text-xs border border-border focus:outline-none focus:ring-1 focus:ring-primary/50 sm:py-1.5"
                    >
                      {hasCustomSelectedApiKey && <option value={selectedApiKey}>{selectedApiKey}</option>}
                      {apiKeys.map((key) => (
                        <option key={key.id} value={key.key}>
                          {key.key}
                        </option>
                      ))}
                    </select>
                  ) : (
                    <span className="min-w-0 rounded bg-surface/40 px-2 py-2 text-xs text-text-muted sm:py-1.5">
                      {cloudEnabled ? "No API keys - Create one in Keys page" : "sk_openproxy (default)"}
                    </span>
                  )}
                </div>

                <div className="grid grid-cols-1 gap-1.5 sm:grid-cols-[8rem_auto_1fr_auto] sm:items-center sm:gap-2">
                  <span className="text-xs font-semibold text-text-main sm:text-right sm:text-sm">
                    Model
                  </span>
                  <span className="material-symbols-outlined hidden text-text-muted text-[14px] sm:inline">
                    arrow_forward
                  </span>
                  <div className="relative w-full min-w-0">
                    <input
                      type="text"
                      value={selectedModel}
                      onChange={(e: ChangeEvent<HTMLInputElement>) => setSelectedModel(e.target.value)}
                      placeholder="provider/model-id"
                      className="w-full min-w-0 pl-2 pr-7 py-2 bg-surface rounded border border-border text-xs focus:outline-none focus:ring-1 focus:ring-primary/50 sm:py-1.5"
                    />
                    {selectedModel && (
                      <button
                        onClick={() => setSelectedModel("")}
                        className="absolute right-1 top-1/2 -translate-y-1/2 p-0.5 text-text-muted hover:text-red-500 rounded transition-colors"
                        title="Clear"
                      >
                        <span className="material-symbols-outlined text-[14px]">close</span>
                      </button>
                    )}
                  </div>
                  <button
                    onClick={() => setModalOpen(true)}
                    disabled={!activeProviders?.length}
                    className={`w-full sm:w-auto rounded border px-2 py-2 text-xs transition-colors sm:py-1.5 whitespace-nowrap sm:shrink-0 ${
                      activeProviders?.length
                        ? "bg-surface border-border text-text-main hover:border-primary cursor-pointer"
                        : "opacity-50 cursor-not-allowed border-border"
                    }`}
                  >
                    Select
                  </button>
                </div>
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

              <div className="grid grid-cols-1 gap-2 sm:flex sm:items-center">
                <Button onClick={() => void handleApply()} disabled={applying || !selectedModel} className="w-full sm:w-auto">
                  {applying ? "Applying..." : "Apply Settings"}
                </Button>
                {status?.hasOpenProxy && (
                  <Button onClick={() => void handleReset()} disabled={restoring} variant="secondary" className="w-full sm:w-auto">
                    {restoring ? "Resetting..." : "Reset Settings"}
                  </Button>
                )}
                <Button onClick={() => setShowManualConfigModal(true)} variant="secondary" className="w-full sm:w-auto">
                  <span className="material-symbols-outlined text-[18px] mr-1">content_copy</span>
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
        onSelect={handleModelSelect}
        selectedModel={selectedModel}
        activeProviders={activeProviders}
      />

      <ManualConfigModal
        isOpen={showManualConfigModal}
        onClose={() => setShowManualConfigModal(false)}
        toolName={tool.name}
        configs={getManualConfigs()}
      />
    </Card>
  );
}
