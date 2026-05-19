"use client";

import { cn } from "@/shared/utils/cn";
import React from "react";
import type { ButtonProps } from "@/types";

/**
 * Claude editorial Button.
 *
 * Variants map to the Claude design system:
 *   primary           -> coral CTA (the signature voltage)
 *   secondary         -> cream-canvas pill with hairline border
 *   outline           -> ink-bordered pill (quiet alt for dense pages)
 *   ghost             -> inline text-button (no chrome)
 *   danger            -> destructive (warning red)
 *   success           -> confirm (semantic green)
 *   primary-on-dark   -> coral pill that sits on dark navy surfaces
 *   secondary-on-dark -> dark-elevated pill for use over dark surfaces
 *
 * Radius is `rounded-mini-md` (8px) — the Claude spec's standard button
 * radius — not a full pill. Buttons stay 36–44px tall depending on size.
 */
const variants = {
  primary:
    "bg-brand-coral text-on-primary hover:bg-brand-coral-active disabled:bg-primary-disabled disabled:text-muted-soft",
  secondary:
    "bg-canvas text-ink border border-hairline hover:border-ink/40 hover:bg-surface-soft disabled:border-hairline disabled:text-muted",
  outline:
    "bg-transparent text-ink border border-ink/80 hover:bg-ink/5 disabled:border-hairline disabled:text-muted",
  ghost:
    "bg-transparent text-ink hover:bg-surface-card disabled:text-muted",
  danger:
    "bg-[color:var(--color-danger)] text-white hover:opacity-90 disabled:bg-hairline disabled:text-muted",
  success:
    "bg-success-text text-white hover:opacity-90 disabled:bg-hairline disabled:text-muted",
  "primary-on-dark":
    "bg-brand-coral text-on-primary hover:bg-brand-coral-active disabled:bg-surface-dark-elevated disabled:text-on-dark-soft",
  "secondary-on-dark":
    "bg-surface-dark-elevated text-on-dark border border-white/10 hover:bg-surface-dark-soft disabled:text-on-dark-soft",
};

const sizes = {
  sm: "h-8 px-3 text-[12px] font-medium",
  md: "h-10 px-4 text-[14px] font-medium",
  lg: "h-12 px-6 text-[15px] font-medium",
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
        "inline-flex items-center justify-center gap-2 rounded-mini-md leading-none",
        "transition-colors duration-150 ease-out cursor-pointer tracking-tight",
        "active:scale-[0.99] disabled:cursor-not-allowed disabled:active:scale-100",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand-coral/30 focus-visible:ring-offset-2 focus-visible:ring-offset-canvas",
        variants[variant],
        sizes[size],
        fullWidth && "w-full",
        className,
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
