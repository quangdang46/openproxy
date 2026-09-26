"use client";

import { cn } from "@/shared/utils/cn";
import React from "react";

type SegmentedControlSize = "sm" | "md" | "lg";
type SegmentedControlVariant = "pill" | "segmented" | "underline";

interface SegmentedControlOption {
  value: string;
  label: string;
  icon?: string;
}

interface SegmentedControlProps {
  options?: SegmentedControlOption[];
  value: string;
  onChange: (value: string) => void;
  size?: SegmentedControlSize;
  /**
   * Tab variants:
   *   segmented  -> grouped segmented control (default; the only shape
   *                 9router has)
   *   pill       -> pill-tab (black-fill active, hairline inactive)
   *   underline  -> segmented-tab (underline-style; M2.7 page pattern)
   */
  variant?: SegmentedControlVariant;
  className?: string;
}

export default function SegmentedControl({
  options = [],
  value,
  onChange,
  size = "md",
  variant = "segmented",
  className,
}: SegmentedControlProps) {
  const sizes: Record<SegmentedControlSize, string> = {
    sm: "h-7 text-xs",
    md: "h-9 text-sm",
    lg: "h-11 text-base",
  };

  if (variant === "underline") {
    return (
      <div
        className={cn(
          "inline-flex items-center gap-1 border-b border-hairline-soft",
          className
        )}
      >
        {options.map((option) => {
          const active = value === option.value;
          return (
            <button
              key={option.value}
              onClick={() => onChange(option.value)}
              className={cn(
                "shrink-0 px-4 py-2.5 type-body-sm-medium relative transition-colors",
                "border-b-2 -mb-px",
                active
                  ? "text-ink border-ink"
                  : "text-steel border-transparent hover:text-ink"
              )}
            >
              {option.icon && (
                <span className="material-symbols-outlined text-[16px] mr-1.5 align-middle">
                  {option.icon}
                </span>
              )}
              {option.label}
            </button>
          );
        })}
      </div>
    );
  }

  if (variant === "pill") {
    return (
      <div className={cn("inline-flex items-center gap-2 flex-wrap", className)}>
        {options.map((option) => {
          const active = value === option.value;
          return (
            <button
              key={option.value}
              onClick={() => onChange(option.value)}
              className={cn(
                "shrink-0 px-4 rounded-full font-semibold transition-colors leading-none",
                sizes[size],
                active
                  // text-canvas inverts with bg-ink so the active pill is
                  // dark-on-cream in light mode and cream-on-dark in dark
                  // mode. The previous `text-on-primary` was always #fff,
                  // which collided with the inverted dark-mode ink.
                  ? "bg-ink text-canvas border border-ink"
                  : "bg-canvas text-steel border border-hairline hover:text-ink hover:border-ink/40"
              )}
            >
              {option.icon && (
                <span className="material-symbols-outlined text-[16px] mr-1.5 align-middle">
                  {option.icon}
                </span>
              )}
              {option.label}
            </button>
          );
        })}
      </div>
    );
  }

  // segmented (grouped — 9router's only shape, and the default here)
  return (
    <div
      className={cn(
        "inline-flex items-center p-1 rounded-[10px] overflow-x-auto",
        "bg-surface-2",
        className
      )}
    >
      {options.map((option) => {
        const active = value === option.value;
        return (
          <button
            key={option.value}
            onClick={() => onChange(option.value)}
            className={cn(
              "shrink-0 px-4 rounded-[8px] font-medium transition-all",
              sizes[size],
              active
                ? "bg-surface text-ink shadow-sm"
                : "text-steel hover:text-ink"
            )}
          >
            {option.icon && (
              <span className="material-symbols-outlined text-[16px] mr-1.5 align-middle">
                {option.icon}
              </span>
            )}
            {option.label}
          </button>
        );
      })}
    </div>
  );
}
