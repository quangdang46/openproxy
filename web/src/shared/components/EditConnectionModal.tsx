"use client";

import { useState, useEffect } from "react";
import type { ChangeEvent } from "react";
import Modal from "@/shared/components/Modal";
import Input from "@/shared/components/Input";
import Button from "@/shared/components/Button";
import Badge from "@/shared/components/Badge";
import Select from "@/shared/components/Select";
import { isOpenAICompatibleProvider, isAnthropicCompatibleProvider, AI_PROVIDERS } from "@/shared/constants/providers";
import type { ProviderRegionConfig } from "@/shared/constants/providers";
import type { Provider } from "@/types";

type RegionOption = NonNullable<ProviderRegionConfig["regions"]>[number];

const NONE_PROXY_POOL_VALUE = "__none__";

interface ProviderSpecificData {
  region?: string;
  azureEndpoint?: string;
  apiVersion?: string;
  deployment?: string;
  organization?: string;
  accountId?: string;
  proxyPoolId?: string;
  // AWS Bedrock credential form (registry credentialForm: "aws")
  profile?: string;
  accessKeyId?: string;
  sessionToken?: string;
}

interface Connection {
  id: string;
  name?: string;
  email?: string;
  displayName?: string;
  priority?: number;
  authType?: string;
  provider?: string;
  providerSpecificData?: ProviderSpecificData;
  /** Server-side flag — true if a key is already on file (never includes the secret). */
  hasApiKey?: boolean;
}

interface ProxyPool {
  id: string;
  name: string;
}

interface EditConnectionModalProps {
  isOpen: boolean;
  connection?: Connection;
  proxyPools?: ProxyPool[];
  onSave: (updates: any) => Promise<void> | void;
  onClose: () => void;
}

// A provider that no longer lists the stored region (registry changed, or the
// connection predates the field) falls back to the registry default.
function regionConfig(provider: string | undefined, saved: string | undefined): { list: RegionOption[]; saved: string } {
  const cfg = (provider ? AI_PROVIDERS[provider] : undefined) as (Provider & ProviderRegionConfig) | undefined;
  const list = cfg?.regions ?? [];
  if (list.length === 0) return { list: [], saved: "" };
  const fallback = cfg?.defaultRegion || list[0].id;
  return { list, saved: list.some((r) => r.id === saved) ? (saved as string) : fallback };
}

export default function EditConnectionModal({ isOpen, connection, proxyPools, onSave, onClose }: EditConnectionModalProps) {
  const [formData, setFormData] = useState({
    name: "",
    email: "",
    displayName: "",
    priority: 1,
    apiKey: "",
    proxyPoolId: NONE_PROXY_POOL_VALUE,
  });
  const [azureData, setAzureData] = useState({
    azureEndpoint: "",
    apiVersion: "2024-10-01-preview",
    deployment: "",
    organization: "",
  });
  const [cloudflareData, setCloudflareData] = useState({ accountId: "" });
  const [awsData, setAwsData] = useState({
    profile: "",
    region: "us-east-1",
    accessKeyId: "",
    sessionToken: "",
  });
  const [region, setRegion] = useState("");
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<"success" | "failed" | null>(null);
  const [validating, setValidating] = useState(false);
  const [validationResult, setValidationResult] = useState<"success" | "failed" | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (connection) {
      setFormData({
        name: connection.name || "",
        email: connection.email || "",
        displayName: connection.displayName || "",
        priority: connection.priority || 1,
        apiKey: "",
        proxyPoolId: connection.providerSpecificData?.proxyPoolId || NONE_PROXY_POOL_VALUE,
      });

      if (connection.provider === "azure" && connection.providerSpecificData) {
        setAzureData({
          azureEndpoint: connection.providerSpecificData.azureEndpoint || "",
          apiVersion: connection.providerSpecificData.apiVersion || "2024-10-01-preview",
          deployment: connection.providerSpecificData.deployment || "",
          organization: connection.providerSpecificData.organization || "",
        });
      }
      if (connection.provider === "cloudflare-ai" && connection.providerSpecificData) {
        setCloudflareData({ accountId: connection.providerSpecificData.accountId || "" });
      }
      if (connection.providerSpecificData) {
        const psd = connection.providerSpecificData;
        setAwsData({
          profile: psd.profile || "",
          region: psd.region || "us-east-1",
          accessKeyId: psd.accessKeyId || "",
          sessionToken: psd.sessionToken || "",
        });
      }
      setRegion(regionConfig(connection.provider, connection.providerSpecificData?.region).saved);
      setTestResult(null);
      setValidationResult(null);
    }
  }, [connection]);

  const isOAuth = connection?.authType === "oauth";
  const isAzure = connection?.provider === "azure";
  const isCloudflareAi = connection?.provider === "cloudflare-ai";
  const isAwsCredentialForm =
    !!connection?.provider &&
    (AI_PROVIDERS[connection.provider] as Provider | undefined)?.credentialForm === "aws";
  const isCompatible = connection
    ? (isOpenAICompatibleProvider(connection.provider) || isAnthropicCompatibleProvider(connection.provider))
    : false;
  const providerRegions = regionConfig(connection?.provider, connection?.providerSpecificData?.region).list;

  // A region-aware provider's blob also carries proxyPoolId, baseUrl and friends,
  // so saving a region merges into what is stored instead of replacing it
  // (9router EditConnectionModal.js:71-74).
  const buildRegionSpecificData = (): ProviderSpecificData | undefined => {
    if (providerRegions.length > 0 && region) {
      return { ...(connection?.providerSpecificData || {}), region };
    }
    return undefined;
  };

  // AWS credential form: merge into the stored blob (it also carries
  // proxyPoolId etc.); an emptied field is removed so the user can switch
  // between profile and static-key modes.
  const buildAwsSpecificData = (): ProviderSpecificData | undefined => {
    if (!isAwsCredentialForm) return undefined;
    const merged: ProviderSpecificData = { ...(connection?.providerSpecificData || {}) };
    const set = (key: "profile" | "region" | "accessKeyId" | "sessionToken", value: string) => {
      const v = value.trim();
      if (v) merged[key] = v;
      else delete merged[key];
    };
    set("profile", awsData.profile);
    set("region", awsData.region);
    set("accessKeyId", awsData.accessKeyId);
    set("sessionToken", awsData.sessionToken);
    return merged;
  };
  const awsProfileMode = isAwsCredentialForm && !!awsData.profile.trim();

  const handleTest = async () => {
    if (!connection?.provider) return;
    setTesting(true);
    setTestResult(null);
    try {
      const res = await fetch(`/api/providers/${connection.id}/test`, { method: "POST" });
      const data = await res.json();
      setTestResult(data.valid ? "success" : "failed");
    } catch {
      setTestResult("failed");
    } finally {
      setTesting(false);
    }
  };

  const handleValidate = async () => {
    if (!connection?.provider || (!formData.apiKey && !awsProfileMode)) return;
    setValidating(true);
    setValidationResult(null);
    try {
      const res = await fetch("/api/providers/validate", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          provider: connection.provider,
          apiKey: formData.apiKey,
          ...(isAzure ? { providerSpecificData: azureData } : {}),
          ...(isCloudflareAi ? { providerSpecificData: cloudflareData } : {}),
          ...(isAwsCredentialForm ? { providerSpecificData: buildAwsSpecificData() } : {}),
          // The validator dials the region the user picked, not the default
          // (9router EditConnectionModal.js:104).
          ...(providerRegions.length > 0 ? { providerSpecificData: buildRegionSpecificData() } : {}),
        }),
      });
      const data = await res.json();
      setValidationResult(data.valid ? "success" : "failed");
    } catch {
      setValidationResult("failed");
    } finally {
      setValidating(false);
    }
  };

  const handleSubmit = async () => {
    if (!connection) return;
    setSaving(true);
    try {
      const updates: any = {
        name: formData.name,
        // Secondary identity. Empty string clears on the backend.
        email: formData.email,
        displayName: formData.displayName,
        priority: formData.priority,
        // Proxy pool: null clears binding; string sets it.
        proxyPoolId:
          formData.proxyPoolId === NONE_PROXY_POOL_VALUE ? null : formData.proxyPoolId,
      };
      if (!isOAuth && formData.apiKey) {
        updates.apiKey = formData.apiKey;
        let isValid = validationResult === "success";
        if (!isValid) {
          try {
            setValidating(true);
            setValidationResult(null);
            const res = await fetch("/api/providers/validate", {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({
                provider: connection.provider,
                apiKey: formData.apiKey,
                ...(isAzure ? { providerSpecificData: azureData } : {}),
                ...(isCloudflareAi ? { providerSpecificData: cloudflareData } : {}),
                ...(providerRegions.length > 0 ? { providerSpecificData: buildRegionSpecificData() } : {}),
              }),
            });
            const data = await res.json();
            isValid = !!data.valid;
            setValidationResult(isValid ? "success" : "failed");
          } catch {
            setValidationResult("failed");
          } finally {
            setValidating(false);
          }
        }
        if (isValid) {
          updates.testStatus = "active";
          updates.lastError = null;
          updates.lastErrorAt = null;
        }
      }

      if (isAzure) {
        updates.providerSpecificData = {
          azureEndpoint: azureData.azureEndpoint,
          apiVersion: azureData.apiVersion,
          deployment: azureData.deployment,
          organization: azureData.organization,
        };
      }
      if (isCloudflareAi) {
        updates.providerSpecificData = { accountId: cloudflareData.accountId };
      }
      // Last write wins over the azure/cloudflare branches above, as in 9router:
      // no provider declares both a region and azure config
      // (EditConnectionModal.js:170-173).
      if (providerRegions.length > 0 && region) {
        updates.providerSpecificData = buildRegionSpecificData();
      }
      if (isAwsCredentialForm) {
        updates.providerSpecificData = buildAwsSpecificData();
      }

      await onSave(updates);
    } finally {
      setSaving(false);
    }
  };

  if (!connection) return null;

  return (
    <Modal isOpen={isOpen} title="Edit Connection" onClose={onClose}>
      <div className="flex flex-col gap-4">
        <Input
          label="Name"
          value={formData.name}
          onChange={(e) => setFormData({ ...formData, name: e.target.value })}
          placeholder={isOAuth ? "Account name" : "Production Key"}
        />
        <Input
          label="Display Name"
          value={formData.displayName}
          onChange={(e) => setFormData({ ...formData, displayName: e.target.value })}
          placeholder="Optional secondary label"
          hint="Shown under the primary name when it differs"
        />
        <Input
          label="Email"
          value={formData.email}
          onChange={(e) => setFormData({ ...formData, email: e.target.value })}
          placeholder={isOAuth ? "Account email" : "Optional email"}
          hint={isOAuth ? "OAuth account identity (editable label)" : "Optional identity label"}
        />
        <Input
          label="Priority"
          type="number"
          value={formData.priority}
          onChange={(e) => setFormData({ ...formData, priority: Number.parseInt(e.target.value, 10) || 1 })}
        />

        <Select
          label="Proxy Pool"
          value={formData.proxyPoolId}
          onChange={(e) => setFormData({ ...formData, proxyPoolId: e.target.value })}
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

        {!isOAuth && (
          <>
            <div className="flex gap-2">
              <Input
                label={isAwsCredentialForm ? "AWS Secret Access Key" : "API Key"}
                type="password"
                value={formData.apiKey}
                onChange={(e) => setFormData({ ...formData, apiKey: e.target.value })}
                placeholder={connection.hasApiKey ? "•••••••• (saved — leave blank to keep)" : isAwsCredentialForm ? "AWS secret access key" : "Enter new API key"}
                hint={isAwsCredentialForm
                  ? awsProfileMode
                    ? "Profile mode is active — the secret key is not needed. Leave blank to keep any stored key."
                    : connection.hasApiKey
                      ? "A key is already stored. Leave blank to keep it, or type a new one to replace."
                      : "Required for static-key mode (profile mode does not need it)."
                  : connection.hasApiKey
                    ? "A key is already stored. Leave blank to keep it, or type a new one to replace."
                    : "Required."}
                className="flex-1"
              />
              <div className="pt-6">
                <Button
                  onClick={connection.hasApiKey && !formData.apiKey ? handleTest : handleValidate}
                  disabled={(!formData.apiKey && !connection.hasApiKey && !awsProfileMode) || validating || saving || testing}
                  variant="secondary"
                >
                  {validating || testing ? "Checking..." : "Check"}
                </Button>
              </div>
            </div>
            {connection.hasApiKey && !formData.apiKey && (
              <Badge variant="info">Key on file</Badge>
            )}
            {validationResult && (
              <Badge variant={validationResult === "success" ? "success" : "error"}>
                {validationResult === "success" ? "Valid" : "Invalid"}
              </Badge>
            )}
            {testResult && !validationResult && (
              <Badge variant={testResult === "success" ? "success" : "error"}>
                {testResult === "success" ? "Saved key works" : "Saved key failed test"}
              </Badge>
            )}
          </>
        )}

        {isAzure && (
          <div className="bg-sidebar/50 p-4 rounded-lg border border-accent/20">
            <h3 className="font-semibold mb-3 text-sm">Azure OpenAI Configuration</h3>
            <div className="flex flex-col gap-3">
              <Input
                label="Azure Endpoint"
                value={azureData.azureEndpoint}
                onChange={(e) => setAzureData({ ...azureData, azureEndpoint: e.target.value })}
                placeholder="https://your-resource.openai.azure.com"
                hint="Your Azure OpenAI resource endpoint URL"
              />
              <Input
                label="Deployment Name"
                value={azureData.deployment}
                onChange={(e) => setAzureData({ ...azureData, deployment: e.target.value })}
                placeholder="gpt-4"
                hint="The deployment name in your Azure resource"
              />
              <Input
                label="API Version"
                value={azureData.apiVersion}
                onChange={(e) => setAzureData({ ...azureData, apiVersion: e.target.value })}
                placeholder="2024-10-01-preview"
                hint="Azure OpenAI API version to use"
              />
              <Input
                label="Organization"
                value={azureData.organization}
                onChange={(e) => setAzureData({ ...azureData, organization: e.target.value })}
                placeholder="Organization ID"
                hint="Required for billing"
              />
            </div>
          </div>
        )}

        {isAwsCredentialForm && (
          <div className="bg-sidebar/50 p-4 rounded-lg border border-accent/20 flex flex-col gap-3">
            <h3 className="font-semibold text-sm">AWS Bedrock Credentials</h3>
            <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
              <Input
                label="Profile"
                value={awsData.profile}
                onChange={(e) => setAwsData({ ...awsData, profile: e.target.value })}
                placeholder="default"
              />
              <Input
                label="Region"
                value={awsData.region}
                onChange={(e) => setAwsData({ ...awsData, region: e.target.value })}
                placeholder="us-east-1"
              />
            </div>
            <Input
              label="Access Key ID"
              value={awsData.accessKeyId}
              onChange={(e) => setAwsData({ ...awsData, accessKeyId: e.target.value })}
              placeholder="AKIA… / ASIA…"
            />
            <Input
              label="Session Token"
              type="password"
              value={awsData.sessionToken}
              onChange={(e) => setAwsData({ ...awsData, sessionToken: e.target.value })}
              placeholder="(temporary ASIA… keys only)"
            />
            <p className="text-xs text-text-muted leading-relaxed">
              SSO / profile (recommended): fill in <b>Profile</b> and <b>Region</b>, leave the secret access key empty, then run{" "}
              <code>aws sso login --profile &lt;name&gt;</code>. Static keys: the AWS secret access key goes in the key field above,
              the key id in <b>Access Key ID</b>, and <b>Session Token</b> for temporary (ASIA…) keys. A profile, if set, takes
              precedence over static keys.
            </p>
          </div>
        )}

        {providerRegions.length > 0 && (
          <Select
            label="Region"
            value={region}
            onChange={(e: ChangeEvent<HTMLSelectElement>) => setRegion(e.target.value)}
            options={providerRegions.map((r) => ({ value: r.id, label: r.label }))}
          />
        )}

        {!isCompatible && !isAzure && !isCloudflareAi && (
          <div className="flex items-center gap-3">
            <Button onClick={handleTest} variant="secondary" disabled={testing}>
              {testing ? "Testing..." : "Test Connection"}
            </Button>
            {testResult && (
              <Badge variant={testResult === "success" ? "success" : "error"}>
                {testResult === "success" ? "Valid" : "Failed"}
              </Badge>
            )}
          </div>
        )}

        <div className="flex gap-2">
          <Button onClick={handleSubmit} fullWidth disabled={saving}>{saving ? "Saving..." : "Save"}</Button>
          <Button onClick={onClose} variant="ghost" fullWidth>Cancel</Button>
        </div>
      </div>
    </Modal>
  );
}
