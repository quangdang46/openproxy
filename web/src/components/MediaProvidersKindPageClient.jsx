"use client";

import { useState, useEffect } from "react";
import { Card, Button, Badge } from "@/shared/components";
import { MEDIA_PROVIDER_KINDS } from "@/shared/constants/providers";

export default function MediaProvidersKindPageClient() {
  const [kind, setKind] = useState("");
  const [providers, setProviders] = useState([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    // Get kind from URL
    const pathParts = window.location.pathname.split("/");
    const kindFromPath = pathParts[pathParts.length - 1];
    setKind(kindFromPath);
    fetchProviders(kindFromPath);
  }, []);

  const fetchProviders = async (kindParam) => {
    try {
      const res = await fetch(`/api/media-providers/${kindParam}`);
      if (res.ok) {
        const data = await res.json();
        setProviders(data.providers || []);
      }
    } catch (error) {
      console.error("Failed to fetch providers:", error);
    } finally {
      setLoading(false);
    }
  };

  const kindInfo = MEDIA_PROVIDER_KINDS.find(k => k.id === kind);

  if (loading) {
    return <div className="text-center py-12">Loading...</div>;
  }

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">{kindInfo?.label || kind}</h1>
        <Button>Add Provider</Button>
      </div>

      {providers.length === 0 ? (
        <Card padding="lg">
          <div className="text-center py-12">
            <p className="text-text-muted">No {kind} providers configured</p>
            <p className="text-sm text-text-muted mt-2">Add a {kind} provider to get started</p>
          </div>
        </Card>
      ) : (
        <div className="grid gap-4">
          {providers.map((provider) => (
            <Card key={provider.id} padding="lg">
              <div className="flex items-center justify-between">
                <div>
                  <h3 className="font-semibold">{provider.name}</h3>
                  <p className="text-sm text-text-muted">{provider.description || "No description"}</p>
                </div>
                <Badge variant={provider.active ? "success" : "neutral"}>
                  {provider.active ? "Active" : "Inactive"}
                </Badge>
              </div>
            </Card>
          ))}
        </div>
      )}
    </div>
  );
}
