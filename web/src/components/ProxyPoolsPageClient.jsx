"use client";

import { useState, useEffect } from "react";
import { Card, Button, Badge } from "@/shared/components";

export default function ProxyPoolsPageClient() {
  const [pools, setPools] = useState([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    fetchPools();
  }, []);

  const fetchPools = async () => {
    try {
      const res = await fetch("/api/proxy-pools");
      if (res.ok) {
        const data = await res.json();
        setPools(data.pools || []);
      }
    } catch (error) {
      console.error("Failed to fetch proxy pools:", error);
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
        <h1 className="text-2xl font-bold">Proxy Pools</h1>
        <Button>Add Pool</Button>
      </div>

      {pools.length === 0 ? (
        <Card padding="lg">
          <div className="text-center py-12">
            <p className="text-text-muted">No proxy pools configured</p>
            <p className="text-sm text-text-muted mt-2">Create a proxy pool to manage your proxies</p>
          </div>
        </Card>
      ) : (
        <div className="grid gap-4">
          {pools.map((pool) => (
            <Card key={pool.id} padding="lg">
              <div className="flex items-center justify-between">
                <div>
                  <h3 className="font-semibold">{pool.name}</h3>
                  <p className="text-sm text-text-muted">{pool.description || "No description"}</p>
                </div>
                <Badge variant={pool.active ? "success" : "neutral"}>
                  {pool.active ? "Active" : "Inactive"}
                </Badge>
              </div>
            </Card>
          ))}
        </div>
      )}
    </div>
  );
}
