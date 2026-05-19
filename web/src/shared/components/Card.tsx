"use client";

import { cn } from "@/shared/utils/cn";
import React from "react";

type CardPadding = "none" | "xs" | "sm" | "md" | "lg" | "xl";
type CardRadius = "lg" | "xl" | "xxl" | "xxxl" | "hero";
type CardTone = "canvas" | "cream" | "dark" | "coral";

interface CardProps extends React.HTMLAttributes<HTMLDivElement> {
  children?: React.ReactNode;
  title?: string;
  subtitle?: string;
  icon?: string;
  action?: React.ReactNode;
  padding?: CardPadding;
  /**
   * Hierarchical radius scale per Claude editorial spec:
   *   lg    = 12px (content + product cards — default)
   *   xl    = 16px (hero illustration container, larger marquee components)
   *   xxl   = 20px (oversized feature panels)
   *   xxxl  = 24px (decorative product tiles)
   *   hero  = 32px (full-bleed promotional cards)
   */
  radius?: CardRadius;
  /**
   * Surface mode. The Claude design system alternates between three
   * tones to create page-by-page pacing:
   *   canvas - default cream canvas card (with hairline border)
   *   cream  - light cream surface-card (no border, slightly darker
   *            than canvas — used in feature grids)
   *   dark   - dark navy product-mockup surface (code editor mocks,
   *            model showcase, pre-footer CTAs)
   *   coral  - full-bleed coral CTA card (button typically inverts to
   *            a cream pill against the coral fill)
   */
  tone?: CardTone;
  /**
   * Render the title in the editorial slab-serif display face
   * (Instrument Serif / Tiempos Headline substitute). The default keeps
   * the title in the humanist sans for dense dashboard pages.
   */
  serifTitle?: boolean;
  hover?: boolean;
  elev?: boolean;
  className?: string;
}

interface CardSectionProps extends React.HTMLAttributes<HTMLDivElement> {
  children?: React.ReactNode;
  className?: string;
}

interface CardRowProps extends React.HTMLAttributes<HTMLDivElement> {
  children?: React.ReactNode;
  className?: string;
}

interface CardListItemProps extends React.HTMLAttributes<HTMLDivElement> {
  children?: React.ReactNode;
  actions?: React.ReactNode;
  className?: string;
}

const paddings: Record<CardPadding, string> = {
  none: "",
  xs: "p-3",
  sm: "p-4",
  md: "p-5",
  lg: "p-6",
  xl: "p-8",
};

const radii: Record<CardRadius, string> = {
  lg: "rounded-mini-lg",
  xl: "rounded-mini-xl",
  xxl: "rounded-mini-xxl",
  xxxl: "rounded-mini-xxxl",
  hero: "rounded-hero",
};

const toneSurfaces: Record<CardTone, string> = {
  canvas: "bg-canvas border border-hairline text-ink",
  cream: "bg-surface-card border border-hairline-soft text-ink",
  dark: "bg-surface-dark text-on-dark border border-transparent",
  coral: "bg-brand-coral text-on-primary border border-transparent",
};

const toneIconBg: Record<CardTone, string> = {
  canvas: "bg-surface-card text-ink",
  cream: "bg-canvas text-ink",
  dark: "bg-surface-dark-elevated text-on-dark",
  coral: "bg-white/15 text-on-primary",
};

const toneSubtitle: Record<CardTone, string> = {
  canvas: "text-body",
  cream: "text-body",
  dark: "text-on-dark-soft",
  coral: "text-on-primary/85",
};

const toneHover: Record<CardTone, string> = {
  canvas: "hover:border-ink/30 hover:shadow-soft",
  cream: "hover:bg-surface-cream-strong",
  dark: "hover:bg-surface-dark-elevated",
  coral: "hover:bg-brand-coral-active",
};

export default function Card({
  children,
  title,
  subtitle,
  icon,
  action,
  padding = "md",
  radius = "lg",
  tone = "canvas",
  serifTitle = false,
  hover = false,
  elev = false,
  className,
  ...props
}: CardProps) {
  return (
    <div
      className={cn(
        toneSurfaces[tone],
        radii[radius],
        elev ? "shadow-card" : "shadow-none",
        hover && `${toneHover[tone]} transition-colors cursor-pointer`,
        paddings[padding],
        className,
      )}
      {...props}
    >
      {(title || action) && (
        <div className="flex items-center justify-between mb-4">
          <div className="flex items-center gap-3">
            {icon && (
              <div className={cn("p-2 rounded-mini-md", toneIconBg[tone])}>
                <span className="material-symbols-outlined text-[20px]">{icon}</span>
              </div>
            )}
            <div>
              {title && (
                <h3
                  className={cn(
                    "leading-tight",
                    serifTitle
                      ? "font-serif text-[22px] tracking-[-0.01em] font-normal"
                      : "font-semibold text-[15px]",
                  )}
                >
                  {title}
                </h3>
              )}
              {subtitle && (
                <p className={cn("text-[13px] mt-0.5", toneSubtitle[tone])}>{subtitle}</p>
              )}
            </div>
          </div>
          {action}
        </div>
      )}
      {children}
    </div>
  );
}

Card.Section = function CardSection({ children, className, ...props }: CardSectionProps) {
  return (
    <div
      className={cn(
        "p-4 rounded-mini-md",
        "bg-surface-card border border-hairline-soft",
        className,
      )}
      {...props}
    >
      {children}
    </div>
  );
};

Card.Row = function CardRow({ children, className, ...props }: CardRowProps) {
  return (
    <div
      className={cn(
        "p-3 -mx-3 px-3 transition-colors",
        "border-b border-hairline-soft last:border-b-0",
        "hover:bg-surface-soft",
        className,
      )}
      {...props}
    >
      {children}
    </div>
  );
};

Card.ListItem = function CardListItem({
  children,
  actions,
  className,
  ...props
}: CardListItemProps) {
  return (
    <div
      className={cn(
        "group flex items-center justify-between p-3 -mx-3 px-3",
        "border-b border-hairline-soft last:border-b-0",
        "hover:bg-surface-soft transition-colors",
        className,
      )}
      {...props}
    >
      <div className="flex-1 min-w-0">{children}</div>
      {actions && (
        <div className="flex items-center gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
          {actions}
        </div>
      )}
    </div>
  );
};
