"use client";

import { useState, useEffect } from "react";
import { Card, Button, Input } from "@/shared/components";
import { OAUTH_PROVIDERS, APIKEY_PROVIDERS } from "@/shared/constants/providers";

export default function ProvidersNewPageClient() {
  const [selectedProvider, setSelectedProvider] = useState(null);
  const [loading, setLoading] = useState(false);

  const handleConnect = async () => {
    if (!selectedProvider) return;

    setLoading(true);
    try {
      const res = await fetch("/api/providers/connect", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ provider: selectedProvider }),
      });

      if (res.ok) {
        window.location.href = "/dashboard/providers";
      }
    } catch (error) {
      console.error("Failed to connect provider:", error);
    } finally {
      setLoading(false);
    }
  };

  const allProviders = [...Object.values(OAUTH_PROVIDERS), ...Object.values(APIKEY_PROVIDERS)];

  return (
    <div className="flex flex-col gap-6">
      <h1 className="text-2xl font-bold">Add New Provider</h1>

      <Card padding="lg">
        <div className="flex flex-col gap-4">
          <label className="text-sm font-medium text-text-muted">Select Provider</label>
          <select
            value={selectedProvider || ""}
            onChange={(e) => setSelectedProvider(e.target.value)}
            className="w-full p-3 border border-border rounded-lg bg-bg-subtle focus:outline-none focus:ring-2 focus:ring-primary/50"
          >
            <option value="">Choose a provider...</option>
            {allProviders.map((provider) => (
              <option key={provider.id} value={provider.id}>
                {provider.name}
              </option>
            ))}
          </select>

          <Button onClick={handleConnect} disabled={loading || !selectedProvider}>
            {loading ? "Connecting..." : "Connect Provider"}
          </Button>
        </div>
      </Card>
    </div>
  );
}
