"use client";

import { useState } from "react";
import { Card, Button } from "@/shared/components";
import { OAUTH_PROVIDERS, APIKEY_PROVIDERS } from "@/shared/constants/providers";

export default function ProvidersNewPageClient() {
  const [selectedProvider, setSelectedProvider] = useState<string | null>(null);

  const handleConnect = () => {
    if (!selectedProvider) return;

    // OAuth providers: kick off the OAuth flow on the server.
    if (selectedProvider in OAUTH_PROVIDERS) {
      window.location.href = `/api/oauth/${encodeURIComponent(selectedProvider)}/start`;
      return;
    }

    // API-key providers: send the user to the providers list with a
    // query param so the page can open the right "Add API key" panel.
    window.location.href = `/dashboard/providers?add=${encodeURIComponent(selectedProvider)}`;
  };

  const allProviders = [
    ...Object.values(OAUTH_PROVIDERS),
    ...Object.values(APIKEY_PROVIDERS),
  ];

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

          <Button onClick={handleConnect} disabled={!selectedProvider}>
            Connect Provider
          </Button>
        </div>
      </Card>
    </div>
  );
}
