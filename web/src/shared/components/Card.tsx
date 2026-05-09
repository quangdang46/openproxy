"use client";

import { cn } from "@/shared/utils/cn";
import React from "react";

type CardPadding = "none" | "xs" | "sm" | "md" | "lg" | "xl";
type CardRadius = "lg" | "xl" | "xxl" | "xxxl" | "hero";

interface CardProps extends React.HTMLAttributes<HTMLDivElement> {
  children?: React.ReactNode;
  title?: string;
  subtitle?: string;
  icon?: string;
  action?: React.ReactNode;
  padding?: CardPadding;
  /**
   * MiniMax radius scale.
   *   lg    = 12px (recommendation tile)
   *   xl    = 16px (default; standard feature card / docs card)
   *   xxl   = 20px (larger feature panel)
   *   xxxl  = 24px (AI product tile)
   *   hero  = 32px (vibrant gradient product card)
   */
  radius?: CardRadius;
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

export default function Card({
  children,
  title,
  subtitle,
  icon,
  action,
  padding = "md",
  radius = "xl",
  hover = false,
  elev = false,
  className,
  ...props
}: CardProps) {
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

  return (
    <div
      className={cn(
        // MiniMax docs/feature card: flat-with-borders, white canvas.
        "bg-canvas border border-hairline",
        radii[radius],
        elev ? "shadow-card" : "shadow-none",
        hover && "hover:border-ink/30 hover:shadow-soft transition-colors cursor-pointer",
        paddings[padding],
        className
      )}
      {...props}
    >
      {(title || action) && (
        <div className="flex items-center justify-between mb-4">
          <div className="flex items-center gap-3">
            {icon && (
              <div className="p-2 rounded-mini-md bg-surface-base text-slate">
                <span className="material-symbols-outlined text-[20px]">{icon}</span>
              </div>
            )}
            <div>
              {title && (
                <h3 className="text-ink font-semibold text-[15px] leading-tight">{title}</h3>
              )}
              {subtitle && (
                <p className="text-[13px] text-slate mt-0.5">{subtitle}</p>
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
        "bg-surface-soft border border-hairline-soft",
        className
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
        className
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
        className
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
