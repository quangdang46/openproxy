"use client";

import { CAPACITY_META, type CapacityKey, type ModelCaps } from "@/shared/constants/models";
import Tooltip from "./Tooltip";

interface CapacityBadgesProps {
  caps?: ModelCaps | null;
  className?: string;
  /** Force a single color class for all badges (default: per-cap color). */
  colorOverride?: string;
  /** Icon font-size in px (default 16). */
  size?: number;
}

// Render small icon badges for a model's capabilities (only those set true).
export default function CapacityBadges({
  caps,
  className = "",
  colorOverride,
  size = 16,
}: CapacityBadgesProps) {
  if (!caps) return null;
  const active = (Object.keys(CAPACITY_META) as CapacityKey[]).filter((k) => caps[k]);
  if (active.length === 0) return null;

  return (
    <span className={`inline-flex items-center gap-0.5 ${className}`}>
      {active.map((k) => (
        <Tooltip key={k} text={`${CAPACITY_META[k].label} — ${CAPACITY_META[k].desc}`}>
          <span
            className={`material-symbols-outlined leading-none cursor-help ${colorOverride || CAPACITY_META[k].color}`}
            style={{ fontSize: `${size}px` }}
          >
            {CAPACITY_META[k].icon}
          </span>
        </Tooltip>
      ))}
    </span>
  );
}
