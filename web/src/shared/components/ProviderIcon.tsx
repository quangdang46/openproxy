"use client";

import { useState } from "react";
import React from "react";
import { markProviderIconMissing } from "@/shared/utils/providerIcon";

interface ProviderIconProps {
  src?: string;
  alt?: string;
  size?: number;
  className?: string;
  fallbackText?: string;
  fallbackColor?: string;
  /** Provider id — recorded in the session 404 cache on error (9router parity). */
  providerId?: string;
}

export default function ProviderIcon({
  src,
  alt,
  size = 32,
  className = "",
  fallbackText = "?",
  fallbackColor,
  providerId,
}: ProviderIconProps) {
  const [errored, setErrored] = useState(false);

  if (!src || errored) {
    return (
      <span
        className={`inline-flex items-center justify-center font-bold rounded-lg ${className}`.trim()}
        style={{
          width: size,
          height: size,
          color: fallbackColor,
          fontSize: Math.max(10, Math.floor(size * 0.38)),
        }}
      >
        {fallbackText}
      </span>
    );
  }

  return (
    <img
      src={src}
      alt={alt}
      width={size}
      height={size}
      className={className}
      onError={() => {
        if (providerId) markProviderIconMissing(providerId);
        setErrored(true);
      }}
    />
  );
}
