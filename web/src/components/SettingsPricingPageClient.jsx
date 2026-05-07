"use client";

import { useState, useEffect } from "react";
import { Card, Button, Badge } from "@/shared/components";

// Simple pricing plans for demo
const PRICING_PLANS = [
  {
    id: "free",
    name: "Free",
    price: 0,
    features: [
      "Basic model access",
      "100K tokens/month",
      "Community support",
    ],
  },
  {
    id: "pro",
    name: "Pro",
    price: 20,
    features: [
      "All models access",
      "1M tokens/month",
      "Priority support",
      "API access",
    ],
  },
  {
    id: "enterprise",
    name: "Enterprise",
    price: 100,
    features: [
      "Unlimited access",
      "Custom models",
      "Dedicated support",
      "SLA guarantee",
    ],
  },
];

export default function PricingSettingsPage() {
  const [currentPlan, setCurrentPlan] = useState(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    fetchCurrentPlan();
  }, []);

  const fetchCurrentPlan = async () => {
    try {
      const res = await fetch("/api/pricing/current");
      if (res.ok) {
        const data = await res.json();
        setCurrentPlan(data.plan);
      }
    } catch (error) {
      console.error("Failed to fetch current plan:", error);
    } finally {
      setLoading(false);
    }
  };

  if (loading) {
    return <div className="text-center py-12">Loading...</div>;
  }

  return (
    <div className="flex flex-col gap-6">
      <h1 className="text-2xl font-bold">Pricing</h1>

      <div className="grid gap-6 md:grid-cols-3">
        {PRICING_PLANS.map((plan) => (
          <Card
            key={plan.id}
            padding="lg"
            className={currentPlan?.id === plan.id ? "border-primary" : ""}
          >
            <div className="flex flex-col gap-4">
              <div>
                <h3 className="font-semibold text-lg">{plan.name}</h3>
                <p className="text-3xl font-bold mt-2">${plan.price}/month</p>
              </div>

              <ul className="flex flex-col gap-2">
                {plan.features.map((feature, index) => (
                  <li key={index} className="text-sm text-text-muted flex items-center gap-2">
                    <span className="material-symbols-outlined text-green-500">check_circle</span>
                    {feature}
                  </li>
                ))}
              </ul>

              <Button
                variant={currentPlan?.id === plan.id ? "secondary" : "primary"}
                disabled={currentPlan?.id === plan.id}
              >
                {currentPlan?.id === plan.id ? "Current Plan" : "Upgrade"}
              </Button>
            </div>
          </Card>
        ))}
      </div>
    </div>
  );
}
