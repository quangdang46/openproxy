"use client";

import { cn } from "@/shared/utils/cn";
import React from "react";

/**
 * MiniMax Badge variants:
 *   default  -> generic neutral chip on surface
 *   primary  -> coral product chip
 *   success  -> badge-success (pale-green confirmation)
 *   warning  -> warm warning chip (legacy)
 *   error    -> red error chip (legacy)
 *   info     -> brand-blue informational chip (legacy)
 *   new      -> badge-new (coral "NEW" / "Live")
 *   beta     -> badge-beta (pale-blue "BETA")
 *   code     -> badge-code (inline code-style chip, square corners)
 */
type BadgeVariant =
  | "default"
  | "primary"
  | "success"
  | "warning"
  | "error"
  | "info"
  | "new"
  | "beta"
  | "code";
type BadgeSize = "sm" | "md" | "lg";

interface BadgeProps {
  children?: React.ReactNode;
  variant?: BadgeVariant;
  size?: BadgeSize;
  dot?: boolean;
  icon?: string;
  className?: string;
}

const variants: Record<BadgeVariant, string> = {
  default: "bg-surface-base text-slate",
  primary: "bg-brand-coral/10 text-brand-coral",
  success: "bg-success-bg text-success-text",
  warning: "bg-yellow-500/12 text-yellow-700 dark:text-yellow-400",
  error: "bg-[color:var(--color-danger)]/12 text-[color:var(--color-danger)]",
  info: "bg-brand-blue-200 text-brand-blue-deep",
  new: "bg-brand-coral text-on-dark",
  beta: "bg-brand-blue-200 text-brand-blue-deep",
  code: "bg-brand-blue-200 text-brand-blue-deep",
};

const sizes: Record<BadgeSize, string> = {
  sm: "px-2 py-0.5 text-[10px] tracking-wide",
  md: "px-2.5 py-1 text-[11px] tracking-wide",
  lg: "px-3 py-1.5 text-[13px]",
};

export default function Badge({
  children,
  variant = "default",
  size = "md",
  dot = false,
  icon,
  className,
}: BadgeProps) {
  // `code` variant uses square corners (rounded-mini-sm) per spec; all
  // other badge variants use full pill.
  const radius = variant === "code" ? "rounded-mini-sm" : "rounded-full";

  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 font-semibold leading-none",
        radius,
        variants[variant],
        sizes[size],
        className
      )}
    >
      {dot && (
        <span
          className={cn(
            "size-1.5 rounded-full",
            variant === "success" && "bg-success-text",
            variant === "warning" && "bg-yellow-500",
            variant === "error" && "bg-[color:var(--color-danger)]",
            variant === "info" && "bg-brand-blue-deep",
            variant === "primary" && "bg-brand-coral",
            variant === "new" && "bg-on-dark",
            variant === "beta" && "bg-brand-blue-deep",
            variant === "code" && "bg-brand-blue-deep",
            variant === "default" && "bg-stone"
          )}
        />
      )}
      {icon && <span className="material-symbols-outlined text-[14px]">{icon}</span>}
      {children}
    </span>
  );
}
