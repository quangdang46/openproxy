"use client";

import { useEffect, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";

/**
 * Render children into `document.body` via a React portal.
 *
 * Use for overlays that must escape ancestor `overflow`/`transform`/
 * `z-index` stacking contexts (tooltips, dropdowns, popovers) so they are
 * never clipped by scrollable parents. The mounted guard keeps SSR/hydration
 * safe — nothing renders until the client is ready.
 */
export default function Portal({ children }: { children: ReactNode }) {
  const [mounted, setMounted] = useState(false);

  useEffect(() => {
    setMounted(true);
  }, []);

  if (!mounted || typeof document === "undefined") return null;
  return createPortal(children, document.body);
}
