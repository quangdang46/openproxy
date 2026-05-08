"use client";

import { cn } from "@/shared/utils/cn";
import React from "react";

type SegmentedControlSize = "sm" | "md" | "lg";

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
  className?: string;
}

export default function SegmentedControl({
  options = [],
  value,
  onChange,
  size = "md",
  className,
}: SegmentedControlProps) {
  const sizes: Record<SegmentedControlSize, string> = {
    sm: "h-7 text-xs",
    md: "h-9 text-sm",
    lg: "h-11 text-base",
  };

  return (
    <div
      className={cn(
        "inline-flex items-center p-1 rounded-[10px] overflow-x-auto",
        "bg-surface-2",
        className
      )}
    >
      {options.map((option) => (
        <button
          key={option.value}
          onClick={() => onChange(option.value)}
          className={cn(
            "shrink-0 px-4 rounded-[8px] font-medium transition-all",
            sizes[size],
            value === option.value
              ? "bg-surface text-text-main shadow-sm"
              : "text-text-muted hover:text-text-main"
          )}
        >
          {option.icon && (
            <span className="material-symbols-outlined text-[16px] mr-1.5">
              {option.icon}
            </span>
          )}
          {option.label}
        </button>
      ))}
    </div>
  );
}
