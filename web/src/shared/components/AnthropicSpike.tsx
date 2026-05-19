"use client";

import React from "react";
import { cn } from "@/shared/utils/cn";

interface AnthropicSpikeProps {
  size?: number;
  className?: string;
  ariaLabel?: string;
}

/**
 * Anthropic-style 4-spoke radial spike glyph.
 *
 * Used as the brand wordmark prefix and as an inline content marker.
 * It's intentionally a small, geometric asterisk-like mark — never
 * inverted to white-within-the-wordmark per the brand do's/don'ts.
 *
 * Color is inherited via `currentColor`; pass a Tailwind `text-*` class to
 * tint it.
 */
export default function AnthropicSpike({
  size = 18,
  className,
  ariaLabel,
}: AnthropicSpikeProps) {
  return (
    <svg
      role={ariaLabel ? "img" : "presentation"}
      aria-label={ariaLabel}
      aria-hidden={ariaLabel ? undefined : true}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="currentColor"
      className={cn("inline-block shrink-0", className)}
    >
      <path d="M11.0 2.2 L13.0 2.2 L13.4 10.0 L20.0 8.4 L20.5 10.3 L13.6 11.45 L20.5 13.7 L20.0 15.6 L13.4 14.0 L13.0 21.8 L11.0 21.8 L10.6 14.0 L4.0 15.6 L3.5 13.7 L10.4 11.45 L3.5 10.3 L4.0 8.4 L10.6 10.0 Z" />
    </svg>
  );
}
