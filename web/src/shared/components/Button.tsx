"use client";

import { cn } from "@/shared/utils/cn";
import React from "react";
import type { ButtonProps } from "@/types";

/**
 * MiniMax Button — pill-shaped, two-tier dominance.
 *
 * Variants map to the MiniMax design system:
 *   primary    -> button-primary       (black-pill, dominant CTA)
 *   secondary  -> button-secondary     (outlined-pill, paired with primary)
 *   outline    -> button-tertiary      (white-fill quieter pill)
 *   ghost      -> button-link          (inline text-button)
 *   danger     -> red destructive pill (kept from legacy palette)
 *   success    -> green confirm pill   (kept from legacy palette)
 */
const variants = {
  primary:
    "bg-ink text-on-primary hover:bg-charcoal disabled:bg-hairline disabled:text-muted",
  secondary:
    "bg-transparent text-ink border border-ink hover:bg-ink/5 disabled:border-hairline disabled:text-muted",
  outline:
    "bg-canvas text-ink border border-hairline hover:border-ink/40 hover:bg-surface-soft disabled:border-hairline disabled:text-muted",
  ghost:
    "bg-transparent text-ink hover:bg-surface-soft disabled:text-muted",
  danger:
    "bg-[color:var(--color-danger)] text-white hover:opacity-90 disabled:bg-hairline disabled:text-muted",
  success:
    "bg-success-text text-white hover:opacity-90 disabled:bg-hairline disabled:text-muted",
};

const sizes = {
  sm: "h-8 px-3 text-[12px] font-semibold",
  md: "h-9 px-4 text-[13px] font-semibold",
  lg: "h-11 px-6 text-[14px] font-semibold",
};

export default function Button({
  children,
  variant = "primary",
  size = "md",
  icon,
  iconRight,
  disabled = false,
  loading = false,
  fullWidth = false,
  className,
  ...props
}: ButtonProps) {
  return (
    <button
      className={cn(
        // MiniMax button: pill-shaped, tight tracking, weight 600.
        "inline-flex items-center justify-center gap-2 rounded-full leading-none",
        "transition-colors duration-150 ease-out cursor-pointer tracking-tight",
        "active:scale-[0.98] disabled:cursor-not-allowed disabled:active:scale-100",
        variants[variant],
        sizes[size],
        fullWidth && "w-full",
        className
      )}
      disabled={disabled || loading}
      {...props}
    >
      {loading ? (
        <span className="material-symbols-outlined animate-spin text-[18px]">progress_activity</span>
      ) : icon ? (
        <span className="material-symbols-outlined text-[18px]">{icon}</span>
      ) : null}
      {children}
      {iconRight && !loading && (
        <span className="material-symbols-outlined text-[18px]">{iconRight}</span>
      )}
    </button>
  );
}
