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

      {/* Info Section — 9router parity: how pricing works */}
      <Card className="p-6">
        <h2 className="mb-4 text-xl font-semibold">How Pricing Works</h2>
        <div className="space-y-3 text-sm text-text-muted">
          <p>
            <strong>Cost Calculation:</strong> Costs are calculated based on token usage and pricing rates.
            Each request&apos;s cost is determined by: (input_tokens × input_rate) + (output_tokens × output_rate) + (cached_tokens × cached_rate)
          </p>
          <p>
            <strong>Pricing Format:</strong> All rates are in <strong>dollars per million tokens</strong> ($/1M tokens).
            Example: An input rate of 2.50 means $2.50 per 1,000,000 input tokens.
          </p>
          <p>
            <strong>Token Types:</strong>
          </p>
          <ul className="list-disc list-inside ml-4 space-y-1">
            <li><strong>Input:</strong> Standard prompt tokens</li>
            <li><strong>Output:</strong> Completion/response tokens</li>
            <li><strong>Cached:</strong> Cached input tokens (typically 50% of input rate)</li>
            <li><strong>Reasoning:</strong> Special reasoning/thinking tokens (fallback to output rate)</li>
            <li><strong>Cache Creation:</strong> Tokens used to create cache entries (fallback to input rate)</li>
          </ul>
          <p>
            <strong>Custom Pricing:</strong> You can override default pricing for specific models.
            Reset to defaults anytime to restore standard rates.
          </p>
        </div>
      </Card>

      {/* Current Pricing Overview — top-5 + View Full Details (9router parity) */}
      <Card className="p-6">
        <div className="mb-4 flex items-center justify-between">
          <h2 className="text-xl font-semibold">Current Pricing Overview</h2>
          <button
            onClick={() => setShowModal(true)}
            className="text-primary hover:underline text-sm"
          >
            View Full Details
          </button>
        </div>

        {loading ? (
          <div className="py-4 text-center text-text-muted">Loading pricing data...</div>
        ) : currentPricing ? (
          <div className="space-y-3">
            {Object.keys(currentPricing).slice(0, 5).map((provider) => (
              <div key={provider} className="text-sm">
                <span className="font-semibold">{provider.toUpperCase()}:</span>{" "}
                <span className="text-text-muted">
                  {Object.keys(currentPricing[provider]).length} models
                </span>
              </div>
            ))}
            {Object.keys(currentPricing).length > 5 && (
              <div className="text-sm text-text-muted">
                + {Object.keys(currentPricing).length - 5} more providers
              </div>
            )}
          </div>
        ) : (
          <div className="text-text-muted">No pricing data available</div>
        )}
      </Card>

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
