"use client";

import { useState, useEffect } from "react";
import { Card, Button, Badge } from "@/shared/components";

export default function MediaProvidersKindIdPageClient() {
  const [provider, setProvider] = useState(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    // Get kind and id from URL
    const pathParts = window.location.pathname.split("/");
    const idFromPath = pathParts[pathParts.length - 1];
    const kindFromPath = pathParts[pathParts.length - 2];
    fetchProvider(kindFromPath, idFromPath);
  }, []);

  const fetchProvider = async (kind, id) => {
    try {
      const res = await fetch(`/api/media-providers/${kind}/${id}`);
      if (res.ok) {
        const data = await res.json();
        setProvider(data);
      }
    } catch (error) {
      console.error("Failed to fetch provider:", error);
    } finally {
      setLoading(false);
    }
  };

  if (loading) {
    return <div className="text-center py-12">Loading...</div>;
  }

  if (!provider) {
    return (
      <div className="flex flex-col gap-6">
        <h1 className="text-2xl font-bold">Provider Not Found</h1>
        <Card padding="lg">
          <p className="text-text-muted">The requested provider could not be found.</p>
        </Card>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">{provider.name}</h1>
        <Badge variant={provider.active ? "success" : "neutral"}>
          {provider.active ? "Active" : "Inactive"}
        </Badge>
      </div>

      <Card padding="lg">
        <div className="flex flex-col gap-4">
          <div>
            <label className="text-sm font-medium text-text-muted">Description</label>
            <p className="text-lg">{provider.description || "No description"}</p>
          </div>
          <div>
            <label className="text-sm font-medium text-text-muted">Type</label>
            <p className="text-lg">{provider.type}</p>
          </div>
          <div className="flex gap-2">
            <Button>Edit</Button>
            <Button variant="secondary">Delete</Button>
          </div>
        </div>
      </Card>
    </div>
  );
}
