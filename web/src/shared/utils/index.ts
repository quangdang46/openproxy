// Shared Utils - Export all
export { cn } from "./cn";
export * as api from "./api";

import { v4 as uuidv4 } from "uuid";

/**
 * Generate unique ID (UUID v4)
 * @returns UUID v4 string
 */
export const generateId = uuidv4;

/**
 * Extract error code / tag from an error message (401, 429, AUTH, QUOTA...).
 * Prefers explicit HTTP codes; falls back to phrase classification so friendly
 * messages like "You exceeded your current quota..." still badge as QUOTA.
 */
export function getErrorCode(lastError: string | null | undefined): string | null {
  if (!lastError) return null;
  const match = lastError.match(/\b([45]\d{2})\b/);
  if (match) return match[1];

  const lower = lastError.toLowerCase();
  if (
    lower.includes("quota") ||
    lower.includes("balance") ||
    lower.includes("insufficient") ||
    lower.includes("payment required") ||
    lower.includes("billing")
  ) {
    return "QUOTA";
  }
  if (lower.includes("rate limit") || lower.includes("too many requests")) {
    return "429";
  }
  if (
    lower.includes("invalid api key") ||
    lower.includes("credentials") ||
    lower.includes("unauthorized") ||
    lower.includes("auth")
  ) {
    return "AUTH";
  }
  if (
    lower.includes("not supported") ||
    lower.includes("model not found") ||
    lower.includes("does not exist")
  ) {
    return "MODEL";
  }
  if (
    lower.includes("upstream") ||
    lower.includes("bad gateway") ||
    lower.includes("unavailable") ||
    lower.includes("overloaded") ||
    lower.includes("timeout") ||
    lower.includes("timed out")
  ) {
    return "5XX";
  }
  return "ERR";
}

/**
 * Short human label for provider-card error badges.
 * e.g. "2 Errors (QUOTA)" instead of dumping raw upstream text.
 */
export function getErrorBadgeLabel(
  errorCount: number,
  errorCode: string | null | undefined,
): string {
  const n = errorCount > 0 ? errorCount : 1;
  const unit = n === 1 ? "Error" : "Errors";
  if (!errorCode || errorCode === "ERR") return `${n} ${unit}`;
  return `${n} ${unit} (${errorCode})`;
}

/**
 * Get relative time string (e.g. "5 min ago")
 * @param isoDate - ISO date string
 * @returns Relative time
 */
export function getRelativeTime(isoDate: string | null | undefined): string {
  if (!isoDate) return "";
  const diff = Date.now() - new Date(isoDate).getTime();
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}
export * from "./connectionStatus";
export * from "./providerCustomModels";
export * from "./thinkingLevels";
