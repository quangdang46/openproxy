"use client";

import { useState, useCallback, useRef } from "react";

/**
 * Hook for copy to clipboard with feedback
 * @param resetDelay - Time in ms before resetting copied state (default: 2000)
 * @returns {{ copied: string|null, copy: (text: string, id?: string) => void }}
 */
export function useCopyToClipboard(resetDelay: number = 2000) {
  const [copied, setCopied] = useState<string | null>(null);
  const timeoutRef = useRef<NodeJS.Timeout | null>(null);

  const copy = useCallback((text: string, id: string = "default") => {
    const write = async () => {
      if (navigator?.clipboard?.writeText) {
        await navigator.clipboard.writeText(text);
      } else {
        const textarea = document.createElement("textarea");
        textarea.value = text;
        textarea.style.position = "fixed";
        textarea.style.opacity = "0";
        document.body.appendChild(textarea);
        textarea.select();
        document.execCommand("copy");
        document.body.removeChild(textarea);
      }
    };
    write();
    setCopied(id);

    if (timeoutRef.current) {
      clearTimeout(timeoutRef.current);
    }

    timeoutRef.current = setTimeout(() => {
      setCopied(null);
    }, resetDelay);
  }, [resetDelay]);

  return { copied, copy };
}
