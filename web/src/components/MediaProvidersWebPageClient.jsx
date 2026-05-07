"use client";

import { useState, useEffect } from "react";
import { Card, Button, Badge } from "@/shared/components";

export default function MediaProvidersWebPageClient() {
  const [providers, setProviders] = useState([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    fetchProviders();
  }, []);

  const fetchProviders = async () => {
    try {
      const res = await fetch("/api/media-providers/web");
      if (res.ok) {
        const data = await res.json();
        setProviders(data.providers || []);
      }
    } catch (error) {
      console.error("Failed to fetch web providers:", error);
    } finally {
      setLoading(false);
    }
  };

  if (loading) {
    return <div className="text-center py-12">Loading...</div>;
  }

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">Web Search & Fetch</h1>
        <Button>Add Provider</Button>
      </div>

      {providers.length === 0 ? (
        <Card padding="lg">
          <div className="text-center py-12">
            <p className="text-text-muted">No web providers configured</p>
            <p className="text-sm text-text-muted mt-2">Add a web search or fetch provider</p>
          </div>
        </Card>
      ) : (
        <div className="grid gap-4">
          {providers.map((provider) => (
            <Card key={provider.id} padding="lg">
              <div className="flex items-center justify-between">
                <div>
                  <h3 className="font-semibold">{provider.name}</h3>
                  <p className="text-sm text-text-muted">{provider.type}</p>
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
