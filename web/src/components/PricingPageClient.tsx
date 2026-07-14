"use client";

import { useEffect, useState } from "react";
import { Card, Button } from "@/shared/components";
import PricingModal from "@/shared/components/PricingModal";

/**
 * Dedicated pricing settings page — 9router parity for /dashboard/settings/pricing.
 * Replaces the previous Astro shell that always-opened PricingModal with invalid className.
 */
export default function PricingPageClient() {
  const [showModal, setShowModal] = useState(false);
  const [currentPricing, setCurrentPricing] = useState<Record<string, any> | null>(null);
  const [loading, setLoading] = useState(true);

  const loadPricing = async () => {
    setLoading(true);
    try {
      const response = await fetch("/api/pricing");
      if (response.ok) {
        const data = await response.json();
        setCurrentPricing(data);
      }
    } catch (error) {
      console.error("Failed to load pricing:", error);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void loadPricing();
  }, []);

  const getModelCount = () => {
    if (!currentPricing) return 0;
    let count = 0;
    for (const provider of Object.keys(currentPricing)) {
      const models = currentPricing[provider];
      if (models && typeof models === "object") {
        count += Object.keys(models).length;
      }
    }
    return count;
  };

  const providers = currentPricing ? Object.keys(currentPricing).sort() : [];

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-6">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h1 className="text-2xl font-semibold text-text-main sm:text-3xl">Pricing Settings</h1>
          <p className="mt-1 text-sm text-text-muted">
            Configure pricing rates for cost tracking and calculations
          </p>
        </div>
        <Button onClick={() => setShowModal(true)} icon="edit">
          Edit Pricing
        </Button>
      </div>

      <div className="grid grid-cols-1 gap-4 md:grid-cols-3">
        <Card className="p-4">
          <div className="text-xs font-semibold uppercase text-text-muted">Total Models</div>
          <div className="mt-1 text-2xl font-bold">{loading ? "…" : getModelCount()}</div>
        </Card>
        <Card className="p-4">
          <div className="text-xs font-semibold uppercase text-text-muted">Providers</div>
          <div className="mt-1 text-2xl font-bold">{loading ? "…" : providers.length}</div>
        </Card>
        <Card className="p-4">
          <div className="text-xs font-semibold uppercase text-text-muted">Storage</div>
          <div className="mt-1 text-sm font-medium text-text-muted">/api/pricing</div>
        </Card>
      </div>

      <Card className="p-4">
        <h2 className="mb-3 text-lg font-semibold">Providers with pricing</h2>
        {loading ? (
          <p className="text-sm text-text-muted">Loading…</p>
        ) : providers.length === 0 ? (
          <p className="text-sm text-text-muted">
            No custom pricing yet. Click <span className="font-medium">Edit Pricing</span> to configure rates.
          </p>
        ) : (
          <ul className="flex flex-wrap gap-2">
            {providers.map((p) => (
              <li
                key={p}
                className="rounded-full border border-border bg-surface px-3 py-1 text-xs font-medium text-text-main"
              >
                {p}
                <span className="ml-1 text-text-muted">
                  ({Object.keys(currentPricing?.[p] || {}).length})
                </span>
              </li>
            ))}
          </ul>
        )}
      </Card>

      <PricingModal
        isOpen={showModal}
        onClose={() => setShowModal(false)}
        onSave={() => {
          void loadPricing();
        }}
      />
    </div>
  );
}
