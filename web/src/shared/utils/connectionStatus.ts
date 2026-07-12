export type ConnectionStatusVariant = "default" | "success" | "error";

/**
 * Map connection active flag + effective test status to a Badge variant.
 * Mirrors 9router `shared/utils/connectionStatus.js`.
 */
export function getStatusVariant(
  isActive: boolean | undefined | null,
  effectiveStatus: string | undefined | null,
): ConnectionStatusVariant {
  if (isActive === false) return "default";
  if (effectiveStatus === "active" || effectiveStatus === "success") return "success";
  if (
    effectiveStatus === "error" ||
    effectiveStatus === "expired" ||
    effectiveStatus === "unavailable"
  ) {
    return "error";
  }
  return "default";
}
