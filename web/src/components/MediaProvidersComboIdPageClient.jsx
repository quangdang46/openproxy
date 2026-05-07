"use client";

import { useState, useEffect } from "react";
import { Card, Button, Badge } from "@/shared/components";

export default function MediaProvidersComboIdPageClient() {
  const [combo, setCombo] = useState(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    // Get id from URL
    const pathParts = window.location.pathname.split("/");
    const idFromPath = pathParts[pathParts.length - 1];
    fetchCombo(idFromPath);
  }, []);

  const fetchCombo = async (id) => {
    try {
      const res = await fetch(`/api/media-providers/combo/${id}`);
      if (res.ok) {
        const data = await res.json();
        setCombo(data);
      }
    } catch (error) {
      console.error("Failed to fetch combo:", error);
    } finally {
      setLoading(false);
    }
  };

  if (loading) {
    return <div className="text-center py-12">Loading...</div>;
  }

  if (!combo) {
    return (
      <div className="flex flex-col gap-6">
        <h1 className="text-2xl font-bold">Combo Not Found</h1>
        <Card padding="lg">
          <p className="text-text-muted">The requested combo could not be found.</p>
        </Card>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">{combo.name}</h1>
        <Badge variant={combo.active ? "success" : "neutral"}>
          {combo.active ? "Active" : "Inactive"}
        </Badge>
      </div>

      <Card padding="lg">
        <div className="flex flex-col gap-4">
          <div>
            <label className="text-sm font-medium text-text-muted">Description</label>
            <p className="text-lg">{combo.description || "No description"}</p>
          </div>
          <div>
            <label className="text-sm font-medium text-text-muted">Providers</label>
            <div className="flex flex-wrap gap-2 mt-2">
              {combo.providers?.map((provider) => (
                <Badge key={provider.id} variant="neutral">
                  {provider.name}
                </Badge>
              ))}
            </div>
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
