"use client";

import { useState, useEffect } from "react";
// import { usePathname } from "next/navigation";
import { initRuntimeI18n, reloadTranslations } from "./runtime";

interface RuntimeI18nProviderProps {
  children: React.ReactNode;
}

export function RuntimeI18nProvider({ children }: RuntimeI18nProviderProps) {
  const [pathname, setPathname] = useState("");
  const [mounted, setMounted] = useState(false);

  useEffect(() => {
    setMounted(true);
    setPathname(window.location.pathname);
  }, []);

  useEffect(() => {
    initRuntimeI18n();
  }, []);

  // Re-process DOM when route changes
  useEffect(() => {
    if (pathname) {
      // Double RAF to ensure React has committed changes to DOM
      requestAnimationFrame(() => {
        requestAnimationFrame(() => {
          reloadTranslations();
        });
      });
    }
  }, [pathname]);

  return <>{children}</>;
}
