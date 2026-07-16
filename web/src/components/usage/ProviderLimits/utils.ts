import { getModelsByProviderId } from "@/shared/constants/models";

// ─── Pagination / filter / sort constants (9router ProviderLimits contract) ───
export const CONNECTIONS_PAGE_SIZE = 20;
export const ACCOUNT_PAGE_SIZE_OPTIONS = [10, 20, 50, 100] as const;
export const ACCOUNT_PAGE_SIZE_MAX = 500;
export const ACCOUNT_FILTER_OPTIONS = [
  { value: "all", label: "All accounts" },
  { value: "active", label: "Active" },
  { value: "inactive", label: "Turned off" },
] as const;
export const QUOTA_SORT_OPTIONS = [
  { value: "default", label: "Default quota order" },
  { value: "remaining-asc", label: "% quota: low to high" },
  { value: "remaining-desc", label: "% quota: high to low" },
] as const;

export type AccountFilterValue = (typeof ACCOUNT_FILTER_OPTIONS)[number]["value"];
export type QuotaSortMode = (typeof QUOTA_SORT_OPTIONS)[number]["value"];

export interface ConnectionsPagination {
  page: number;
  pageSize: number;
  total: number;
  totalPages: number;
}

export interface ConnectionsTotals {
  eligibleConnections: number;
  providerFilteredConnections: number;
}

export function getConnectionQuotaRemaining(
  connection: { id: string },
  quotaData: Record<string, { quotas?: Array<{ remaining?: number; remainingPercentage?: number; used?: number; total?: number }> }>,
): number {
  const quota = quotaData[connection.id]?.quotas?.[0];
  if (!quota) return Number.POSITIVE_INFINITY;
  if (typeof quota.remaining === "number") return quota.remaining;
  if (typeof quota.remainingPercentage === "number") return quota.remainingPercentage;
  if (typeof quota.used === "number" && typeof quota.total === "number") {
    return calculatePercentage(quota.used, quota.total);
  }
  return Number.POSITIVE_INFINITY;
}

export function sortVisibleConnections<T extends { id: string; provider?: string; name?: string; email?: string }>(
  connections: T[],
  quotaData: Record<string, { quotas?: Array<{ remaining?: number; remainingPercentage?: number; used?: number; total?: number; resetAt?: string | null }> }>,
  expiringFirst: boolean,
  providerFilter: string,
  quotaSortMode: QuotaSortMode | string,
): T[] {
  if (providerFilter === "codex" && quotaSortMode !== "default") {
    return [...connections].sort((a, b) => {
      const remainingA = getConnectionQuotaRemaining(a, quotaData);
      const remainingB = getConnectionQuotaRemaining(b, quotaData);
      const remainingDiff =
        quotaSortMode === "remaining-asc"
          ? remainingA - remainingB
          : remainingB - remainingA;
      if (remainingDiff !== 0) return remainingDiff;
      const labelA = a.name?.trim() || a.email?.trim() || "";
      const labelB = b.name?.trim() || b.email?.trim() || "";
      return labelA.localeCompare(labelB);
    });
  }

  if (!expiringFirst) return connections;

  const getEarliestResetTime = (connection: T): number => {
    const resetTimes = (quotaData[connection.id]?.quotas || [])
      .map((quota) =>
        quota.resetAt
          ? new Date(quota.resetAt).getTime()
          : Number.POSITIVE_INFINITY,
      )
      .filter((time) => Number.isFinite(time));
    return resetTimes.length > 0
      ? Math.min(...resetTimes)
      : Number.POSITIVE_INFINITY;
  };

  return [...connections].sort((a, b) => {
    const expiryDiff = getEarliestResetTime(a) - getEarliestResetTime(b);
    if (expiryDiff !== 0) return expiryDiff;
    return (
      (a.provider || "").localeCompare(b.provider || "") ||
      (a.name?.trim() || a.email?.trim() || "").localeCompare(
        b.name?.trim() || b.email?.trim() || "",
      )
    );
  });
}

export function getConnectionsPageRange(pagination: ConnectionsPagination) {
  if (!pagination.total) {
    return { start: 0, end: 0 };
  }
  const start = (pagination.page - 1) * pagination.pageSize + 1;
  const end = Math.min(pagination.page * pagination.pageSize, pagination.total);
  return { start, end };
}

export function getConnectionsEmptyMessage(
  totals: ConnectionsTotals,
  providerFilter: string,
  accountFilter: string,
) {
  if (!totals.eligibleConnections) {
    return {
      icon: "cloud_off",
      title: "No Providers Connected",
      description:
        "Connect to providers with OAuth to track your API quota limits and usage.",
    };
  }
  if (!totals.providerFilteredConnections) {
    return {
      icon: "filter_alt_off",
      title: "No Accounts Match Current Filters",
      description:
        providerFilter === "all"
          ? "Try changing the account status filter to see more quota trackers."
          : `No ${accountFilter === "inactive" ? "turned off" : accountFilter === "active" ? "active" : "matching"} accounts found for ${providerFilter}.`,
    };
  }
  return {
    icon: "filter_alt_off",
    title: "No Accounts On This Page",
    description:
      "Try moving to another page or refreshing the current filters.",
  };
}

export function getPageSizeLabel(pageSize: number, isCustomPageSize: boolean) {
  return isCustomPageSize ? `Custom: ${pageSize} / page` : `${pageSize} / page`;
}

export function getConnectionsPaginationSummary(pagination: ConnectionsPagination) {
  const { start, end } = getConnectionsPageRange(pagination);
  return `Showing ${start}-${end} of ${pagination.total}`;
}

export function getSafePagination(
  pagination: Partial<ConnectionsPagination> | null | undefined,
  fallbackPageSize: number,
): ConnectionsPagination {
  return {
    page: pagination?.page || 1,
    pageSize: pagination?.pageSize || fallbackPageSize,
    total: pagination?.total ?? 0,
    totalPages: pagination?.totalPages || 1,
  };
}

export function getSafeTotals(
  totals: Partial<ConnectionsTotals> | null | undefined,
  fallbackTotal = 0,
): ConnectionsTotals {
  return {
    eligibleConnections: totals?.eligibleConnections ?? fallbackTotal,
    providerFilteredConnections: totals?.providerFilteredConnections ?? fallbackTotal,
  };
}

export function shouldResetPage(previousValue: string, nextValue: string) {
  return previousValue !== nextValue;
}

export function getPaginationPageValue(
  dataPagination: Partial<ConnectionsPagination> | null | undefined,
  fallbackPage: number,
) {
  return dataPagination?.page || fallbackPage;
}

export function getProviderOptions(dataProviderOptions: string[] | null | undefined) {
  return dataProviderOptions || [];
}

export async function reconcileConnectionsPage(
  fetchConnections: (page?: number) => Promise<unknown>,
  targetPage: number,
) {
  return await fetchConnections(targetPage);
}

/**
 * Get remaining percentage from a normalized quota row
 */
export function getRemainingPercentage(quota: {
  remaining?: number;
  remainingPercentage?: number;
  used?: number;
  total?: number;
} | null | undefined): number {
  if (quota?.remaining !== undefined) {
    return Math.max(0, Math.round(quota.remaining));
  }
  if (quota?.remainingPercentage !== undefined) {
    return Math.round(quota.remainingPercentage);
  }
  return calculatePercentage(quota?.used ?? 0, quota?.total ?? 0);
}

interface QuotaEntry {
  name: string;
  used: number;
  total: number;
  resetAt?: string | null;
  recurring?: boolean;
}

interface NormalizedQuota extends QuotaEntry {
  modelKey?: string;
  remainingPercentage?: number;
  message?: string;
  recurring?: boolean;
}

interface RawQuotaData {
  quotas?: Record<string, QuotaEntry>;
  plan?: string;
  message?: string;
}

interface Model {
  id: string;
}

/**
 * Format ISO date string to countdown format (inspired by vscode-antigravity-cockpit)
 * @param date - ISO date string or Date object
 * @returns Formatted countdown (e.g., "2d 5h 30m", "4h 40m", "15m") or "-"
 */
export function formatResetTime(date: string | Date | null | undefined): string {
  if (!date) return "-";

  try {
    const resetDate = typeof date === "string" ? new Date(date) : date;
    const now = new Date();
    const diffMs = resetDate.getTime() - now.getTime();

    if (diffMs <= 0) return "-";

    const totalMinutes = Math.ceil(diffMs / (1000 * 60));
    
    // < 60 minutes: show only minutes
    if (totalMinutes < 60) {
      return `${totalMinutes}m`;
    }
    
    const totalHours = Math.floor(totalMinutes / 60);
    const remainingMinutes = totalMinutes % 60;
    
    // < 24 hours: show hours and minutes
    if (totalHours < 24) {
      return `${totalHours}h ${remainingMinutes}m`;
    }
    
    // >= 24 hours: show days, hours, and minutes
    const days = Math.floor(totalHours / 24);
    const remainingHours = totalHours % 24;
    return `${days}d ${remainingHours}h ${remainingMinutes}m`;
  } catch (error) {
    return "-";
  }
}

/**
 * Get Tailwind color class based on percentage
 * @param percentage - Remaining percentage (0-100)
 * @returns Color name: "green" | "yellow" | "red"
 */
export function getStatusColor(percentage: number): "green" | "yellow" | "red" {
  if (percentage > 70) return "green";
  if (percentage >= 30) return "yellow";
  return "red"; // 0-29% including 0% (out of quota) - show red
}

/**
 * Get status emoji based on percentage
 * @param percentage - Remaining percentage (0-100)
 * @returns Emoji: "🟢" | "🟡" | "🔴"
 */
export function getStatusEmoji(percentage: number): string {
  if (percentage > 70) return "🟢";
  if (percentage >= 30) return "🟡";
  return "🔴"; // 0-29% including 0% (out of quota) - show red
}

/**
 * Calculate remaining percentage
 * @param used - Used amount
 * @param total - Total amount
 * @returns Remaining percentage (0-100)
 */
export function calculatePercentage(used: number, total: number): number {
  if (!total || total === 0) return 0;
  if (!used || used < 0) return 100;
  if (used >= total) return 0;

  return Math.round(((total - used) / total) * 100);
}

/**
 * Parse provider-specific quota structures into normalized array
 * @param provider - Provider name (github, antigravity, codex, kiro, claude)
 * @param data - Raw quota data from provider
 * @returns Normalized quota objects with { name, used, total, resetAt }
 */
export function parseQuotaData(provider: string, data: RawQuotaData | null | undefined): NormalizedQuota[] {
  if (!data || typeof data !== "object") return [];

  const normalizedQuotas: NormalizedQuota[] = [];

  try {
    switch (provider.toLowerCase()) {
      case "github":
        if (data.quotas) {
          Object.entries(data.quotas).forEach(([name, quota]: [string, QuotaEntry]) => {
            normalizedQuotas.push({
              name,
              used: quota.used || 0,
              total: quota.total || 0,
              resetAt: quota.resetAt || null,
              recurring: (quota as any).recurring !== false,
            });
          });
        }
        break;

      case "antigravity":
        if (data.quotas) {
          Object.entries(data.quotas).forEach(([modelKey, quota]: [string, any]) => {
            normalizedQuotas.push({
              name: quota.displayName || modelKey,
              modelKey: modelKey, // Keep modelKey for sorting
              used: quota.used || 0,
              total: quota.total || 0,
              resetAt: quota.resetAt || null,
              remainingPercentage: quota.remainingPercentage,
              recurring: quota.recurring !== false,
            });
          });
        }
        break;

      case "codex":
        if (data.quotas) {
          Object.entries(data.quotas).forEach(([quotaType, quota]: [string, QuotaEntry]) => {
            normalizedQuotas.push({
              name: quotaType,
              used: quota.used || 0,
              total: quota.total || 0,
              resetAt: quota.resetAt || null,
              recurring: (quota as any).recurring !== false,
            });
          });
        }
        break;

      case "kiro":
        if (data.quotas) {
          Object.entries(data.quotas).forEach(([quotaType, quota]: [string, QuotaEntry]) => {
            normalizedQuotas.push({
              name: quotaType,
              used: quota.used || 0,
              total: quota.total || 0,
              resetAt: quota.resetAt || null,
              recurring: (quota as any).recurring !== false,
            });
          });
        }
        break;

      case "claude":
        if (data.message) {
          // Handle error message case
          normalizedQuotas.push({
            name: "error",
            used: 0,
            total: 0,
            resetAt: null,
            message: data.message,
          });
        } else if (data.quotas) {
          Object.entries(data.quotas).forEach(([name, quota]: [string, QuotaEntry]) => {
            normalizedQuotas.push({
              name,
              used: quota.used || 0,
              total: quota.total || 0,
              resetAt: quota.resetAt || null,
              recurring: (quota as any).recurring !== false,
            });
          });
        }
        break;

      default:
        // Generic fallback for unknown providers
        if (data.quotas) {
          Object.entries(data.quotas).forEach(([name, quota]: [string, QuotaEntry]) => {
            normalizedQuotas.push({
              name,
              used: quota.used || 0,
              total: quota.total || 0,
              resetAt: quota.resetAt || null,
              // Forward recurring so one-shot packs render as "Expires in".
              recurring: (quota as any).recurring !== false,
            });
          });
        }
    }
  } catch (error) {
    console.error(`Error parsing quota data for ${provider}:`, error);
    return [];
  }

  // Sort quotas according to PROVIDER_MODELS order
  const modelOrder = getModelsByProviderId(provider);
  if (modelOrder.length > 0) {
    const orderMap = new Map(modelOrder.map((m: Model, i: number) => [m.id, i]));
    
    normalizedQuotas.sort((a, b) => {
      // Use modelKey for antigravity, otherwise use name
      const keyA = a.modelKey || a.name;
      const keyB = b.modelKey || b.name;
      const orderA = orderMap.get(keyA) ?? 999;
      const orderB = orderMap.get(keyB) ?? 999;
      return orderA - orderB;
    });
  }

  return normalizedQuotas;
}
