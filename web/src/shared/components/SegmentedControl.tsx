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
   * MiniMax tab variants:
   *   pill       -> pill-tab (default; black-fill active, hairline inactive)
   *   segmented  -> grouped segmented control on surface base
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
  variant = "pill",
  className,
}: SegmentedControlProps) {
  const sizes: Record<SegmentedControlSize, string> = {
    sm: "h-7 text-[12px] px-3",
    md: "h-9 text-[13px] px-4",
    lg: "h-11 text-[14px] px-5",
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
                "shrink-0 rounded-full font-semibold transition-colors leading-none",
                sizes[size],
                active
                  ? "bg-ink text-on-primary border border-ink"
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

  // segmented (grouped — legacy default)
  return (
    <div
      className={cn(
        "inline-flex items-center p-1 rounded-mini-md overflow-x-auto",
        "bg-surface-base border border-hairline-soft",
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
              "shrink-0 rounded-mini-sm font-semibold transition-colors leading-none",
              sizes[size],
              active
                ? "bg-canvas text-ink shadow-soft"
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
