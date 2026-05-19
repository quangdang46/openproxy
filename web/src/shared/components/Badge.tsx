"use client";

import { cn } from "@/shared/utils/cn";
import React from "react";

/**
 * Claude editorial Badge variants:
 *   default  -> cream pill on canvas (`surface-card` background)
 *   primary  -> warm coral pill
 *   success  -> warm green confirmation
 *   warning  -> warm amber chip
 *   error    -> warm red destructive chip
 *   info     -> muted slate info chip
 *   new      -> full coral fill with `NEW` / `BETA` semantics
 *   beta     -> teal accent (status-dot tone) used for product previews
 *   code     -> inline code-style chip with cream surface + mono type
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
  default: "bg-surface-card text-body",
  primary: "bg-brand-coral/15 text-brand-coral",
  success: "bg-success-bg text-success-text",
  warning: "bg-accent-amber/20 text-[color:var(--color-warning)]",
  error: "bg-[color:var(--color-danger)]/12 text-[color:var(--color-danger)]",
  info: "bg-surface-card text-body-strong",
  new: "bg-brand-coral text-on-primary",
  beta: "bg-accent-teal/20 text-accent-teal",
  code: "bg-surface-card text-body-strong border border-hairline font-mono",
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
