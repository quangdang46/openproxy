"use client";

import { useState } from "react";
import type { ChangeEvent } from "react";
import { Button, Badge, Input, Modal, Select } from "@/shared/components";

interface ProxyPool {
  id: string;
  name: string;
}

interface AddApiKeyModalProps {
  isOpen: boolean;
  provider?: string;
  providerName?: string;
  isCompatible?: boolean;
  isAnthropic?: boolean;
  authType?: string;
  authHint?: string;
  website?: string;
  proxyPools?: ProxyPool[];
  /** Error message from parent failed save (9router parity). */
  error?: string;
  onSave: (data: any) => Promise<void>;
  onClose: () => void;
}

export default function AddApiKeyModal({ isOpen, provider, providerName, isCompatible, isAnthropic, authType, authHint, website, proxyPools, onSave, onClose, error }: AddApiKeyModalProps): React.ReactNode {
  const NONE_PROXY_POOL_VALUE = "__none__";
  const isOllamaLocal = provider === "ollama-local";
  const isCookie = authType === "cookie";
  const isAzure = provider === "azure";
  const isCloudflareAi = provider === "cloudflare-ai";
  const isXiaomiTokenplan = provider === "xiaomi-tokenplan";
  const isXaiApiKey = provider === "xai" && !isCookie;
  const credentialLabel = isCookie ? "Cookie Value" : "API Key";
  const credentialPlaceholder = isCookie
    ? (provider === "grok-web" ? "sso=xxxxx... or just the raw value" : "eyJhbGciOi...")
    : (isXaiApiKey ? "xai-..." : "");

  const [formData, setFormData] = useState({
    name: "",
    apiKey: "",
    defaultModel: "",
    priority: 1,
    proxyPoolId: NONE_PROXY_POOL_VALUE,
    ollamaHostUrl: "",
  });
  const [azureData, setAzureData] = useState({
    azureEndpoint: "",
    apiVersion: "2024-10-01-preview",
    deployment: "",
    organization: "",
  });
  const [cloudflareData, setCloudflareData] = useState({ accountId: "" });
  const [xiaomiRegion, setXiaomiRegion] = useState<string>("sgp");
  const [validating, setValidating] = useState<boolean>(false);
  const [validationResult, setValidationResult] = useState<"success" | "failed" | null>(null);
  const [saving, setSaving] = useState<boolean>(false);

  // Bulk add: one key per line. Cloudflare uses name|apiKey|accountId.
  // Skipped for Azure/Ollama/xAI single-key flows that need extra fields.
  const supportsBulk = !isOllamaLocal && !isAzure && !isXaiApiKey;
  const bulkPlaceholder = isCloudflareAi
    ? `name1|sk-key1|acc123456\nname2|sk-key2|def789012\nsk-key-only-auto-named`
    : `prod|sk-aaa...\nstaging|sk-bbb...\nsk-ccc...`;
  const [mode, setMode] = useState<"single" | "bulk">("single");
  const [bulkText, setBulkText] = useState<string>("");
  const [bulkResult, setBulkResult] = useState<{ success: number; failed: number } | null>(null);

  const handleBulkSubmit = async (): Promise<void> => {
    if (!provider) return;
    const lines = bulkText
      .split("\n")
      .map((l) => l.trim())
      .filter(Boolean);
    if (!lines.length) return;
    setSaving(true);
    setBulkResult(null);
    let success = 0;
    let failed = 0;
    // POST directly: onSave from the parent closes the modal on success which
    // would interrupt the loop. The parent should refresh on onClose.
    for (let i = 0; i < lines.length; i++) {
      const parts = lines[i].split("|").map((p) => p.trim());
      let name: string;
      let apiKey: string;
      let providerSpecificData: { accountId: string } | undefined;

      if (isCloudflareAi && parts.length >= 3) {
        // name|apiKey|accountId — apiKey may itself contain pipes
        const baseName = parts[0] || "Key";
        const accountId = parts[parts.length - 1];
        apiKey = parts.slice(1, -1).join("|").trim();
        name = `${baseName} ${i + 1}`;
        providerSpecificData = accountId ? { accountId } : undefined;
      } else if (parts.length >= 2) {
        const baseName = parts[0] || "Key";
        apiKey = parts.slice(1).join("|").trim();
        name = `${baseName} ${i + 1}`;
      } else {
        apiKey = parts[0] || "";
        name = `Key ${i + 1}`;
      }
      if (!apiKey) {
        failed++;
        continue;
      }
      try {
        const res = await fetch("/api/providers", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            provider,
            name,
            apiKey,
            priority: 1,
            testStatus: "unknown",
            ...(providerSpecificData ? { providerSpecificData } : {}),
          }),
        });
        if (res.ok) success++;
        else failed++;
      } catch {
        failed++;
      }
    }
    setSaving(false);
    setBulkResult({ success, failed });
  };

  const buildProviderSpecificData = (): any => {
    if (isOllamaLocal && formData.ollamaHostUrl.trim()) {
      return { baseUrl: formData.ollamaHostUrl.trim() };
    }
    if (isAzure) {
      return {
        azureEndpoint: azureData.azureEndpoint,
        apiVersion: azureData.apiVersion,
        deployment: azureData.deployment,
        organization: azureData.organization,
      };
    }
    if (isCloudflareAi) {
      return { accountId: cloudflareData.accountId };
    }
    if (isXiaomiTokenplan) {
      return { region: xiaomiRegion };
    }
    return undefined;
  };

  const handleValidate = async (): Promise<void> => {
    setValidating(true);
    try {
      const res = await fetch("/api/providers/validate", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ provider, apiKey: formData.apiKey, providerSpecificData: buildProviderSpecificData() }),
      });
      const data = await res.json();
      setValidationResult(data.valid ? "success" : "failed");
    } catch {
      setValidationResult("failed");
    } finally {
      setValidating(false);
    }
  };

  const handleSubmit = async (): Promise<void> => {
    if (!provider) return;
    if (!isOllamaLocal && !formData.apiKey) return;
    if (!isOllamaLocal) {
      // Non-ollama providers require a name
      if (!formData.name) return;
    }
    if (isCompatible && !formData.defaultModel.trim()) return;

    setSaving(true);
    try {
      let isValid = false;
      try {
        setValidating(true);
        setValidationResult(null);
        const res = await fetch("/api/providers/validate", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ provider, apiKey: formData.apiKey, providerSpecificData: buildProviderSpecificData() }),
        });
        const data = await res.json();
        isValid = !!data.valid;
        setValidationResult(isValid ? "success" : "failed");
      } catch {
        setValidationResult("failed");
      } finally {
        setValidating(false);
      }

      await onSave({
        name: formData.name || (isOllamaLocal ? "Ollama Local" : ""),
        apiKey: formData.apiKey,
        defaultModel: isCompatible ? formData.defaultModel.trim() : undefined,
        priority: formData.priority,
        proxyPoolId: formData.proxyPoolId === NONE_PROXY_POOL_VALUE ? null : formData.proxyPoolId,
        testStatus: isValid ? "active" : "unknown",
        providerSpecificData: buildProviderSpecificData()
      });
    } finally {
      setSaving(false);
    }
  };

  if (!provider) return null;

  return (
    <Modal isOpen={isOpen} title={`Add ${providerName || provider} ${credentialLabel}`} onClose={onClose}>
      <div className="flex flex-col gap-4">
        {supportsBulk && (
          <div className="flex gap-2">
            <Button
              size="sm"
              variant={mode === "single" ? "primary" : "ghost"}
              onClick={() => {
                setMode("single");
                setBulkResult(null);
              }}
            >
              Single
            </Button>
            <Button
              size="sm"
              variant={mode === "bulk" ? "primary" : "ghost"}
              onClick={() => {
                setMode("bulk");
                setBulkResult(null);
              }}
            >
              Bulk Add
            </Button>
          </div>
        )}

        {supportsBulk && mode === "bulk" && (
          <div className="flex flex-col gap-3">
            <p className="text-xs text-text-muted">
              {isCloudflareAi
                ? <>One key per line. Format: <code>name|apiKey|accountId</code> or just <code>apiKey</code> (auto-named by index).</>
                : <>One key per line. Format: <code>name|apiKey</code> or just <code>apiKey</code> (auto-named by index).</>
              }
            </p>
            <textarea
              className="w-full rounded border border-accent/30 bg-sidebar p-2 text-sm font-mono resize-y min-h-[140px] focus:outline-none focus:ring-1 focus:ring-primary"
              placeholder={bulkPlaceholder}
              value={bulkText}
              onChange={(e: ChangeEvent<HTMLTextAreaElement>) => setBulkText(e.target.value)}
            />
            {bulkResult && (
              <div
                className={`text-sm font-medium ${
                  bulkResult.failed > 0 ? "text-yellow-400" : "text-green-400"
                }`}
              >
                Added {bulkResult.success}
                {bulkResult.failed > 0 ? `, ${bulkResult.failed} failed` : ""}
              </div>
            )}
            <div className="flex gap-2">
              <Button onClick={() => void handleBulkSubmit()} fullWidth disabled={saving || !bulkText.trim()}>
                {saving ? "Adding..." : "Add All Keys"}
              </Button>
              <Button onClick={onClose} variant="ghost" fullWidth>
                Close
              </Button>
            </div>
          </div>
        )}

        {(!supportsBulk || mode === "single") && (
          <>
        <Input
          label="Name"
          value={formData.name}
          onChange={(e: ChangeEvent<HTMLInputElement>) => setFormData({ ...formData, name: e.target.value })}
          placeholder={isOllamaLocal ? "Ollama Local" : "Production Key"}
        />
        {isOllamaLocal && (
          <div className="flex gap-2">
            <Input
              label="Ollama Host URL"
              value={formData.ollamaHostUrl}
              onChange={(e: ChangeEvent<HTMLInputElement>) => setFormData({ ...formData, ollamaHostUrl: e.target.value })}
              placeholder="http://localhost:11434"
              className="flex-1"
            />
            <div className="pt-6">
              <Button onClick={handleValidate} disabled={validating || saving} variant="secondary">
                {validating ? "Checking..." : "Check"}
              </Button>
            </div>
          </div>
        )}
        {!isOllamaLocal && (
          <div className="flex gap-2">
            <Input
              label={credentialLabel}
              type={isCookie ? "text" : "password"}
              value={formData.apiKey}
              onChange={(e: ChangeEvent<HTMLInputElement>) => setFormData({ ...formData, apiKey: e.target.value })}
              placeholder={credentialPlaceholder}
              className="flex-1"
            />
            <div className="pt-6">
              <Button onClick={handleValidate} disabled={!formData.apiKey || validating || saving} variant="secondary">
                {validating ? "Checking..." : "Check"}
              </Button>
            </div>
          </div>
        )}
        {isCookie && authHint && (
          <p className="text-xs text-text-muted">
            {authHint}
            {website && (
              <>
                {" "}
                <a href={website} target="_blank" rel="noopener noreferrer" className="text-primary underline">
                  Open {website.replace(/^https?:\/\//, "")}
                </a>
              </>
            )}
          </p>
        )}
        {isOllamaLocal && (
          <p className="text-xs text-text-muted">
            Leave blank to use <code>http://localhost:11434</code>. For remote Ollama, enter the full host URL (e.g. <code>http://192.168.1.10:11434</code>).
          </p>
        )}
        {validationResult && (
          <Badge variant={validationResult === "success" ? "success" : "error"}>
            {validationResult === "success" ? "Valid" : "Invalid"}
          </Badge>
        )}
        {isCompatible && (
          <Input
            label="Default Model"
            value={formData.defaultModel}
            onChange={(e: ChangeEvent<HTMLInputElement>) => setFormData({ ...formData, defaultModel: e.target.value })}
            placeholder={isAnthropic ? "claude-3-5-sonnet-latest" : "gpt-4o"}
          />
        )}
        {isCompatible && (
          <p className="text-xs text-text-muted">
            Enter the model ID exactly as your compatible endpoint expects it. This model will be saved as the connection default.
          </p>
        )}
        {isXaiApiKey && (
          <div className="bg-sidebar/50 p-4 rounded-lg border border-accent/20">
            <h3 className="font-semibold mb-3 text-sm">xAI API Key</h3>
            <p className="text-xs text-text-muted leading-relaxed">
              Use a direct xAI API key from <a href="https://console.x.ai" target="_blank" rel="noopener noreferrer" className="text-primary underline">console.x.ai</a>.
              This is separate from Grok Build OAuth.
            </p>
          </div>
        )}
        {isCloudflareAi && (
          <div className="bg-sidebar/50 p-4 rounded-lg border border-accent/20">
            <h3 className="font-semibold mb-3 text-sm">Cloudflare Workers AI</h3>
            <Input
              label="Account ID"
              value={cloudflareData.accountId}
              onChange={(e: ChangeEvent<HTMLInputElement>) => setCloudflareData({ ...cloudflareData, accountId: e.target.value })}
              placeholder="abc123def456..."
            />
            <p className="text-xs text-text-muted mt-2">
              Find your Account ID in the right sidebar of <a href="https://dash.cloudflare.com" target="_blank" rel="noopener noreferrer" className="text-primary underline">dash.cloudflare.com</a>
            </p>
          </div>
        )}
        {isXiaomiTokenplan && (
          <div className="bg-sidebar/50 p-4 rounded-lg border border-accent/20">
            <h3 className="font-semibold mb-3 text-sm">Xiaomi MiMo Token Plan Region</h3>
            <Select
              label="Region"
              value={xiaomiRegion}
              onChange={(e: ChangeEvent<HTMLSelectElement>) => setXiaomiRegion(e.target.value)}
              options={[
                { value: "sgp", label: "Singapore" },
                { value: "cn", label: "China" },
                { value: "ams", label: "Europe" },
              ]}
            />
            <p className="text-xs text-text-muted mt-2">
              Token Plan keys are cluster-specific. Select the region matching your subscription.
            </p>
          </div>
        )}
        {isAzure && (
          <div className="bg-sidebar/50 p-4 rounded-lg border border-accent/20">
            <h3 className="font-semibold mb-3 text-sm">Azure OpenAI Configuration</h3>
            <div className="flex flex-col gap-3">
              <Input
                label="Azure Endpoint"
                value={azureData.azureEndpoint}
                onChange={(e: ChangeEvent<HTMLInputElement>) => setAzureData({ ...azureData, azureEndpoint: e.target.value })}
                placeholder="https://your-resource.openai.azure.com"
              />
              <Input
                label="Deployment Name"
                value={azureData.deployment}
                onChange={(e: ChangeEvent<HTMLInputElement>) => setAzureData({ ...azureData, deployment: e.target.value })}
                placeholder="gpt-4"
              />
              <Input
                label="API Version"
                value={azureData.apiVersion}
                onChange={(e: ChangeEvent<HTMLInputElement>) => setAzureData({ ...azureData, apiVersion: e.target.value })}
                placeholder="2024-10-01-preview"
              />
              <Input
                label="Organization"
                value={azureData.organization}
                onChange={(e: ChangeEvent<HTMLInputElement>) => setAzureData({ ...azureData, organization: e.target.value })}
                placeholder="Organization ID"
              />
            </div>
          </div>
        )}

        <Input
          label="Priority"
          type="number"
          value={formData.priority}
          onChange={(e: ChangeEvent<HTMLInputElement>) => setFormData({ ...formData, priority: Number.parseInt(e.target.value) || 1 })}
        />

        <Select
          label="Proxy Pool"
          value={formData.proxyPoolId}
          onChange={(e: ChangeEvent<HTMLSelectElement>) => setFormData({ ...formData, proxyPoolId: e.target.value })}
          options={[
            { value: NONE_PROXY_POOL_VALUE, label: "None" },
            ...(proxyPools || []).map((pool) => ({ value: pool.id, label: pool.name })),
          ]}
          placeholder="None"
        />

        {(proxyPools || []).length === 0 && (
          <p className="text-xs text-text-muted">
            No active proxy pools available. Create one in Proxy Pools page first.
          </p>
        )}

        {error && (
          <p className="text-xs text-red-500 break-words">{error}</p>
        )}

        <p className="text-xs text-text-muted">
          Legacy manual proxy fields are still accepted by API for backward compatibility.
        </p>

        <div className="flex gap-2">
          <Button onClick={handleSubmit} fullWidth disabled={saving || (!isOllamaLocal && (!formData.name || !formData.apiKey)) || (isCompatible && !formData.defaultModel.trim()) || (isAzure && (!azureData.azureEndpoint || !azureData.deployment || !azureData.organization)) || (isCloudflareAi && !cloudflareData.accountId)}>
            {saving ? "Saving..." : "Save"}
          </Button>
          <Button onClick={onClose} variant="ghost" fullWidth>
            Cancel
          </Button>
        </div>
          </>
        )}
      </div>
    </Modal>
  );
}
