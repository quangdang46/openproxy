"use client";

import { useState, useEffect, useCallback, useRef } from "react";
import ProviderIcon from "@/shared/components/ProviderIcon";
import QuotaTable from "./QuotaTable";
import Toggle from "@/shared/components/Toggle";
import { parseQuotaData, calculatePercentage } from "./utils";
import Card from "@/shared/components/Card";
import { EditConnectionModal } from "@/shared/components";
import { patchSettings } from "@/shared/utils/backendApi";
import { ConfirmModal } from "@/shared/components/Modal";
import Tooltip from "@/shared/components/Tooltip";
import { useNotificationStore } from "@/store/notificationStore";
import { USAGE_SUPPORTED_PROVIDERS, USAGE_APIKEY_PROVIDERS } from "@/shared/constants/providers";
import { AUTO_PING_SETTINGS_KEYS } from "@/shared/constants/config";

interface Connection {
  id: string;
  provider: string;
  name?: string;
  email?: string;
  displayName?: string;
  authType?: string;
  isActive?: boolean;
}

interface QuotaDataEntry {
  quotas: any[];
  plan?: string;
  message?: string;
  raw?: any;
}

const REFRESH_INTERVAL_MS = 60000; // 60 seconds
const DEPLETED_QUOTA_THRESHOLD = 5; // percent
const AUTO_REFRESH_STORAGE_KEY = "quotaAutoRefresh";

// Connection is eligible for the quota page when it uses OAuth or is an apikey provider whitelisted for quota
const isUsageEligible = (conn: Connection) =>
  USAGE_SUPPORTED_PROVIDERS.includes(conn.provider) &&
  (conn.authType === "oauth" || USAGE_APIKEY_PROVIDERS.includes(conn.provider));

function getConnectionLabel(connection: Connection): string {
  return (
    connection.name?.trim() ||
    connection.email?.trim() ||
    connection.displayName?.trim() ||
    ""
  );
}

function getConnectionSecondaryLabel(connection: Connection): string | null {
  if (
    connection.name?.trim() &&
    connection.email?.trim() &&
    connection.name.trim() !== connection.email.trim()
  ) {
    return connection.email.trim();
  }
  if (
    connection.name?.trim() &&
    connection.displayName?.trim() &&
    connection.name.trim() !== connection.displayName.trim()
  ) {
    return connection.displayName.trim();
  }
  return null;
}

function getCodexResetCreditCount(quota?: QuotaDataEntry | null): number {
  const value = quota?.raw?.resetCredits?.availableCount;
  const count = typeof value === "number" ? value : Number(value);
  return Number.isFinite(count) ? Math.max(0, count) : 0;
}

function formatCreditDate(value?: string | null): string {
  if (!value) return "N/A";
  const date = new Date(value);
  if (!Number.isFinite(date.getTime())) return "N/A";
  return date.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    year: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

function formatTimeRemaining(value?: string | null): string {
  if (!value) return "N/A";
  const diffMs = new Date(value).getTime() - Date.now();
  if (!Number.isFinite(diffMs)) return "N/A";
  if (diffMs <= 0) return "Expired";
  const totalHours = Math.ceil(diffMs / (60 * 60 * 1000));
  const days = Math.floor(totalHours / 24);
  const hours = totalHours % 24;
  return days > 0 ? `${days}d ${hours}h` : `${hours}h`;
}

interface CodexResetCredit {
  status?: string;
  grantedAt?: string | null;
  expiresAt?: string | null;
}

interface CodexResetCreditsData {
  availableCount?: number;
  credits: CodexResetCredit[];
}

interface ProxyPool {
  id: string;
  name: string;
}

export default function ProviderLimits() {
  const [connections, setConnections] = useState<Connection[]>([]);
  const [quotaData, setQuotaData] = useState<Record<string, QuotaDataEntry>>({});
  const [loading, setLoading] = useState<Record<string, boolean>>({});
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [autoRefresh, setAutoRefresh] = useState<boolean>(() => {
    if (typeof window === "undefined") return true;
    const stored = window.localStorage.getItem(AUTO_REFRESH_STORAGE_KEY);
    return stored === null ? true : stored === "true";
  });
  const [lastUpdated, setLastUpdated] = useState<Date | null>(null);
  const [refreshingAll, setRefreshingAll] = useState<boolean>(false);
  const [countdown, setCountdown] = useState<number>(60);
  const [connectionsLoading, setConnectionsLoading] = useState<boolean>(true);
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const [deleteConfirmId, setDeleteConfirmId] = useState<string | null>(null);
  const notify = useNotificationStore();
  const [togglingId, setTogglingId] = useState<string | null>(null);
  const [resettingLimitId, setResettingLimitId] = useState<string | null>(null);
  const [resetConfirmState, setResetConfirmState] = useState<{
    connection: Connection;
    resetCreditCount: number;
  } | null>(null);
  const [resetCreditsState, setResetCreditsState] = useState<{
    connection: Connection;
    loading: boolean;
    error: string | null;
    data: CodexResetCreditsData | null;
  } | null>(null);
  const [showEditModal, setShowEditModal] = useState<boolean>(false);
  const [selectedConnection, setSelectedConnection] = useState<Connection | null>(null);
  const [proxyPools, setProxyPools] = useState<ProxyPool[]>([]);
  const [providerFilter, setProviderFilter] = useState<string>("all");
  const [expiringFirst, setExpiringFirst] = useState<boolean>(false);
  const [providerMenuOpen, setProviderMenuOpen] = useState<boolean>(false);
  const [bulkToggling, setBulkToggling] = useState<boolean>(false);
  const [autoPingMaps, setAutoPingMaps] = useState<Record<string, Record<string, boolean>>>({ claude: {}, codex: {} });
  const autoPingTooltips: Record<string, string> = {
    claude: "When your 5h quota runs out, auto-sends a request the moment it resets so a new window starts right away.",
    codex: "Auto-starts the next 5h Codex window after reset by sending a tiny gpt-5.5 request.",
  };

  const intervalRef = useRef<NodeJS.Timeout | null>(null);
  const countdownRef = useRef<NodeJS.Timeout | null>(null);

  // Fetch all provider connections
  const fetchConnections = useCallback(async () => {
    try {
      const response = await fetch("/api/providers");
      if (!response.ok) throw new Error("Failed to fetch connections");

      const data = await response.json();
      const connectionList = data.connections || [];
      setConnections(connectionList);
      return connectionList;
    } catch (error) {
      console.error("Error fetching connections:", error);
      setConnections([]);
      return [];
    }
  }, []);

  // Fetch quota for a specific connection
  const fetchQuota = useCallback(async (connectionId: string, provider: string) => {
    setLoading((prev) => ({ ...prev, [connectionId]: true }));
    setErrors((prev) => ({ ...prev, [connectionId]: "" }));

    try {
      console.log(
        `[ProviderLimits] Fetching quota for ${provider} (${connectionId})`,
      );
      const response = await fetch(`/api/usage/${connectionId}`);

      if (!response.ok) {
        const errorData = await response.json().catch(() => ({}));
        const errorMsg = errorData.error || response.statusText;

        // Handle different error types gracefully
        if (response.status === 404) {
          // Connection not found - skip silently
          console.warn(
            `[ProviderLimits] Connection not found for ${provider}, skipping`,
          );
          return;
        }

        if (response.status === 401) {
          // Auth error - show message instead of throwing
          console.warn(
            `[ProviderLimits] Auth error for ${provider}:`,
            errorMsg,
          );
          setQuotaData((prev) => ({
            ...prev,
            [connectionId]: {
              quotas: [],
              message: errorMsg,
            },
          }));
          return;
        }

        throw new Error(`HTTP ${response.status}: ${errorMsg}`);
      }

      const data = await response.json();
      console.log(`[ProviderLimits] Got quota for ${provider}:`, data);

      // Parse quota data using provider-specific parser
      const parsedQuotas = parseQuotaData(provider, data);

      setQuotaData((prev) => ({
        ...prev,
        [connectionId]: {
          quotas: parsedQuotas,
          plan: data.plan || null,
          message: data.message || null,
          raw: data,
        },
      }));
    } catch (error) {
      console.error(
        `[ProviderLimits] Error fetching quota for ${provider} (${connectionId}):`,
        error,
      );
      setErrors((prev) => ({
        ...prev,
        [connectionId]: (error as Error).message || "Failed to fetch quota",
      }));
    } finally {
      setLoading((prev) => ({ ...prev, [connectionId]: false }));
    }
  }, []);

  // Refresh quota for a specific provider
  const refreshProvider = useCallback(
    async (connectionId: string, provider: string) => {
      await fetchQuota(connectionId, provider);
      setLastUpdated(new Date());
    },
    [fetchQuota],
  );

  const handleResetCodexLimit = useCallback(
    async (connectionId: string, provider: string) => {
      if (provider !== "codex" || resettingLimitId) return;

      setResettingLimitId(connectionId);
      setErrors((prev) => {
        const next = { ...prev };
        delete next[connectionId];
        return next;
      });

      try {
        const response = await fetch(
          `/api/usage/${connectionId}/codex-reset-credits`,
          { method: "POST" },
        );
        const result = await response.json().catch(() => ({}));

        if (!response.ok) {
          throw new Error(
            result.message ||
              result.error ||
              result.code ||
              "Failed to reset Codex limit",
          );
        }

        notify.success(
          result.localOnly
            ? "Local Codex rate-limit state cleared"
            : "Codex rate-limit reset credit used",
        );
        await fetchQuota(connectionId, provider);
        setLastUpdated(new Date());
      } catch (error) {
        const message =
          (error as Error).message || "Failed to reset Codex limit";
        setErrors((prev) => ({ ...prev, [connectionId]: message }));
        notify.error(message);
      } finally {
        setResettingLimitId(null);
      }
    },
    [fetchQuota, notify, resettingLimitId],
  );

  const handleViewCodexResetCredits = useCallback(
    async (connection: Connection) => {
      setResetCreditsState({
        connection,
        loading: true,
        error: null,
        data: null,
      });
      try {
        const response = await fetch(
          `/api/usage/${connection.id}/codex-reset-credits`,
          { cache: "no-store" },
        );
        const result = await response.json().catch(() => ({}));
        if (!response.ok) {
          throw new Error(
            result.error ||
              result.message ||
              "Failed to load Codex reset credits",
          );
        }
        const credits: CodexResetCredit[] = Array.isArray(result.credits)
          ? [...result.credits]
          : [];
        credits.sort((a, b) => {
          const aTime = a.expiresAt
            ? new Date(a.expiresAt).getTime()
            : Number.POSITIVE_INFINITY;
          const bTime = b.expiresAt
            ? new Date(b.expiresAt).getTime()
            : Number.POSITIVE_INFINITY;
          return aTime - bTime;
        });
        setResetCreditsState({
          connection,
          loading: false,
          error: null,
          data: {
            availableCount: result.availableCount ?? 0,
            credits,
          },
        });
      } catch (error) {
        setResetCreditsState({
          connection,
          loading: false,
          error:
            (error as Error).message || "Failed to load Codex reset credits",
          data: null,
        });
      }
    },
    [],
  );

  const handleDeleteConnection = useCallback((id: string) => {
    setDeleteConfirmId(id);
  }, []);

  const confirmDeleteConnection = useCallback(async () => {
    const id = deleteConfirmId;
    if (!id) return;
    setDeletingId(id);
    try {
      const res = await fetch(`/api/providers/${id}`, { method: "DELETE" });
      if (res.ok) {
        setQuotaData((prev) => {
          const next = { ...prev };
          delete next[id];
          return next;
        });
        setLoading((prev) => {
          const next = { ...prev };
          delete next[id];
          return next;
        });
        setErrors((prev) => {
          const next = { ...prev };
          delete next[id];
          return next;
        });
        // Re-fetch connection list so filter/counts stay in sync without manual refresh.
        await fetchConnections();
        notify.success("Connection deleted");
      } else {
        notify.error("Failed to delete connection");
      }
    } catch (error) {
      console.error("Error deleting connection:", error);
      notify.error("Failed to delete connection");
    } finally {
      setDeletingId(null);
      setDeleteConfirmId(null);
    }
  }, [deleteConfirmId, notify, fetchConnections]);

  const handleToggleConnectionActive = useCallback(async (id: string, isActive: boolean) => {
    setTogglingId(id);
    try {
      const res = await fetch(`/api/providers/${id}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ isActive }),
      });
      if (res.ok) {
        // Refresh connections list; re-fetch this connection's quota when still active.
        const conns = await fetchConnections();
        const conn = conns.find((c: Connection) => c.id === id);
        if (conn && isActive && isUsageEligible(conn)) {
          await fetchQuota(id, conn.provider);
          setLastUpdated(new Date());
        }
      }
    } catch (error) {
      console.error("Error updating connection status:", error);
    } finally {
      setTogglingId(null);
    }
  }, [fetchConnections, fetchQuota]);

  const handleUpdateConnection = useCallback(
    async (formData: Record<string, any>) => {
      if (!selectedConnection?.id) return;
      const connectionId = selectedConnection.id;
      const provider = selectedConnection.provider;
      try {
        const res = await fetch(`/api/providers/${connectionId}`, {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(formData),
        });
        if (res.ok) {
          await fetchConnections();
          setShowEditModal(false);
          setSelectedConnection(null);
          if (USAGE_SUPPORTED_PROVIDERS.includes(provider)) {
            await fetchQuota(connectionId, provider);
          }
        }
      } catch (error) {
        console.error("Error saving connection:", error);
      }
    },
    [selectedConnection, fetchConnections, fetchQuota],
  );

  useEffect(() => {
    let cancelled = false;
    fetch("/api/proxy-pools?isActive=true", { cache: "no-store" })
      .then((res) => res.json())
      .then((data) => {
        if (!cancelled && data?.proxyPools) {
          setProxyPools(data.proxyPools);
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  // Refresh all providers
  const refreshAll = useCallback(async () => {
    if (refreshingAll) return;

    setRefreshingAll(true);
    setCountdown(60);

    try {
      const conns = await fetchConnections();

      // Filter eligible connections (OAuth + whitelisted apikey)
      const eligibleConnections = conns.filter(isUsageEligible);

      await Promise.all(
        eligibleConnections.map((conn) => fetchQuota(conn.id, conn.provider)),
      );

      setLastUpdated(new Date());
    } catch (error) {
      console.error("Error refreshing all providers:", error);
    } finally {
      setRefreshingAll(false);
    }
  }, [refreshingAll, fetchConnections, fetchQuota]);

  // Initial load: fetch connections first so cards render immediately, then fetch quotas
  useEffect(() => {
    const initializeData = async () => {
      setConnectionsLoading(true);
      const conns = await fetchConnections();
      setConnectionsLoading(false);

      const eligibleConnections = conns.filter(isUsageEligible);

      // Mark all as loading before fetching
      const loadingState: Record<string, boolean> = {};
      eligibleConnections.forEach((conn) => {
        loadingState[conn.id] = true;
      });
      setLoading(loadingState);

      await Promise.all(
        eligibleConnections.map((conn) => fetchQuota(conn.id, conn.provider)),
      );
      setLastUpdated(new Date());
    };

    initializeData();
  }, [fetchConnections, fetchQuota]);

  // Persist auto-refresh preference
  useEffect(() => {
    if (typeof window === "undefined") return;
    window.localStorage.setItem(AUTO_REFRESH_STORAGE_KEY, String(autoRefresh));
  }, [autoRefresh]);

  // Auto-refresh interval
  useEffect(() => {
    if (!autoRefresh) {
      if (intervalRef.current) {
        clearInterval(intervalRef.current);
        intervalRef.current = null;
      }
      if (countdownRef.current) {
        clearInterval(countdownRef.current);
        countdownRef.current = null;
      }
      return;
    }

    // Main refresh interval
    intervalRef.current = setInterval(() => {
      refreshAll();
    }, REFRESH_INTERVAL_MS);

    // Countdown interval
    countdownRef.current = setInterval(() => {
      setCountdown((prev) => {
        if (prev <= 1) return 60;
        return prev - 1;
      });
    }, 1000);

    return () => {
      if (intervalRef.current) clearInterval(intervalRef.current);
      if (countdownRef.current) clearInterval(countdownRef.current);
    };
  }, [autoRefresh, refreshAll]);

  // Pause auto-refresh when tab is hidden (Page Visibility API)
  useEffect(() => {
    const handleVisibilityChange = () => {
      if (document.hidden) {
        if (intervalRef.current) {
          clearInterval(intervalRef.current);
          intervalRef.current = null;
        }
        if (countdownRef.current) {
          clearInterval(countdownRef.current);
          countdownRef.current = null;
        }
      } else if (autoRefresh) {
        // Resume auto-refresh when tab becomes visible
        intervalRef.current = setInterval(refreshAll, REFRESH_INTERVAL_MS);
        countdownRef.current = setInterval(() => {
          setCountdown((prev) => (prev <= 1 ? 60 : prev - 1));
        }, 1000);
      }
    };

    document.addEventListener("visibilitychange", handleVisibilityChange);
    return () => {
      document.removeEventListener("visibilitychange", handleVisibilityChange);
    };
  }, [autoRefresh, refreshAll]);

  // Filter only supported providers (OAuth or whitelisted apikey)
  const filteredConnections = connections.filter(isUsageEligible);

  const providerFilteredConnections = filteredConnections.filter(
    (conn) => providerFilter === "all" || conn.provider === providerFilter,
  );

  const getEarliestResetTime = (conn: Connection): number => {
    const resetTimes = (quotaData[conn.id]?.quotas || [])
      .map((quota: any) => quota.resetAt ? new Date(quota.resetAt).getTime() : Number.POSITIVE_INFINITY)
      .filter((time) => Number.isFinite(time));
    return resetTimes.length > 0 ? Math.min(...resetTimes) : Number.POSITIVE_INFINITY;
  };

  // Sort providers by USAGE_SUPPORTED_PROVIDERS order, then alphabetically.
  // Optionally surface accounts with quotas expiring soonest first.
  const sortedConnections = [...providerFilteredConnections].sort((a, b) => {
    if (expiringFirst) {
      const expiryDiff = getEarliestResetTime(a) - getEarliestResetTime(b);
      if (expiryDiff !== 0) return expiryDiff;
    }
    const orderA = USAGE_SUPPORTED_PROVIDERS.indexOf(a.provider);
    const orderB = USAGE_SUPPORTED_PROVIDERS.indexOf(b.provider);
    if (orderA !== orderB) return orderA - orderB;
    return a.provider.localeCompare(b.provider);
  });

  // Connection is depleted when any quota entry hit the threshold
  const isConnectionDepleted = (conn: Connection): boolean => {
    const quotas = quotaData[conn.id]?.quotas;
    if (!quotas?.length) return false;
    return quotas.some((q: any) => {
      if (!q.total || q.total <= 0) return false;
      return calculatePercentage(q.used, q.total) <= DEPLETED_QUOTA_THRESHOLD;
    });
  };

  const bulkSetActive = useCallback(
    async (targetIds: string[], isActive: boolean) => {
      if (!targetIds.length || bulkToggling) return;
      setBulkToggling(true);
      try {
        await Promise.all(
          targetIds.map((id) =>
            fetch(`/api/providers/${id}`, {
              method: "PUT",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({ isActive }),
            }),
          ),
        );
        setConnections((prev) =>
          prev.map((c) => (targetIds.includes(c.id) ? { ...c, isActive } : c)),
        );
      } catch (error) {
        console.error("Error bulk toggling connections:", error);
      } finally {
        setBulkToggling(false);
      }
    },
    [bulkToggling],
  );

  const toggleAutoPing = useCallback(
    async (connectionId: string, provider: string, on: boolean) => {
      const settingsKey = AUTO_PING_SETTINGS_KEYS[provider as keyof typeof AUTO_PING_SETTINGS_KEYS];
      if (!settingsKey) return;
      const previous = autoPingMaps[provider] || {};
      const nextProviderMap = { ...previous, [connectionId]: on };
      const nextMaps = { ...autoPingMaps, [provider]: nextProviderMap };
      setAutoPingMaps(nextMaps);
      try {
        await patchSettings({ [settingsKey]: nextProviderMap });
      } catch (error) {
        console.error("Error saving auto-ping config:", error);
        setAutoPingMaps((prev) => ({ ...prev, [provider]: previous }));
      }
    },
    [autoPingMaps],
  );

  const handleDisableDepleted = () => {
    const ids = sortedConnections
      .filter((c) => (c.isActive ?? true) && isConnectionDepleted(c))
      .map((c) => c.id);
    bulkSetActive(ids, false);
  };

  const handleEnableAvailable = () => {
    const ids = sortedConnections
      .filter((c) => !(c.isActive ?? true) && !isConnectionDepleted(c))
      .map((c) => c.id);
    bulkSetActive(ids, true);
  };

  const providerOptions = Array.from(new Set(filteredConnections.map((conn) => conn.provider))).sort();
  const selectedProviderLabel = providerFilter === "all" ? "All providers" : providerFilter;

  // Calculate summary stats
  const totalProviders = sortedConnections.length;
  const activeWithLimits = Object.values(quotaData).filter(
    (data) => data?.quotas?.length > 0,
  ).length;

  // Count low quotas (remaining < 30%)
  const lowQuotasCount = Object.values(quotaData).reduce((count, data) => {
    if (!data?.quotas) return count;

    const hasLowQuota = data.quotas.some((quota: any) => {
      const percentage = calculatePercentage(quota.used, quota.total);
      return percentage < 30 && quota.total > 0;
    });

    return count + (hasLowQuota ? 1 : 0);
  }, 0);

  // Empty state
  if (!connectionsLoading && sortedConnections.length === 0) {
    return (
      <Card padding="lg">
        <div className="text-center py-12">
          <span className="material-symbols-outlined text-[64px] text-text-muted opacity-20">
            cloud_off
          </span>
          <h3 className="mt-4 text-lg font-semibold text-text-primary">
            No Providers Connected
          </h3>
          <p className="mt-2 text-sm text-text-muted max-w-md mx-auto">
            Connect to providers with OAuth to track your API quota limits and
            usage.
          </p>
        </div>
      </Card>
    );
  }

  return (
    <div className="space-y-6">
      {/* Header Controls */}
      <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex flex-col gap-1 sm:flex-row sm:items-center sm:gap-3">
          <h2 className="text-xl font-semibold text-text-primary">
            Provider Limits
          </h2>
        </div>

        <div className="flex flex-wrap items-center gap-1.5">
          <div className="relative">
            <button
              type="button"
              onClick={() => setProviderMenuOpen((prev) => !prev)}
              className="flex h-8 items-center justify-between gap-1 rounded-lg border border-black/10 bg-black/[0.02] px-2 text-xs text-text-primary transition-colors hover:bg-black/5 dark:border-white/10 dark:bg-white/[0.03] dark:hover:bg-white/10"
              aria-haspopup="menu"
              aria-expanded={providerMenuOpen}
              title="Filter quota providers"
            >
              <span className="flex min-w-0 items-center gap-1.5">
                {providerFilter === "all" ? (
                  <span className="material-symbols-outlined text-[14px] text-text-muted">apps</span>
                ) : (
                  <ProviderIcon
                    src={`/providers/${providerFilter}.png`}
                    alt={providerFilter}
                    size={18}
                    className="size-[18px] rounded object-contain"
                    fallbackText={providerFilter.slice(0, 2).toUpperCase()}
                  />
                )}
                <span className="truncate capitalize hidden lg:inline">{selectedProviderLabel}</span>
              </span>
              <span className="material-symbols-outlined text-[14px] text-text-muted">expand_more</span>
            </button>

            {providerMenuOpen && (
              <>
                <button
                  type="button"
                  className="fixed inset-0 z-30 bg-transparent"
                  aria-label="Close provider filter"
                  onClick={() => setProviderMenuOpen(false)}
                />
                <div className="absolute left-0 z-40 mt-2 w-64 overflow-hidden rounded-2xl border border-black/10 bg-surface/95 p-1.5 shadow-xl shadow-black/10 backdrop-blur dark:border-white/10 dark:bg-surface/95 sm:w-72">
                  <button
                    type="button"
                    onClick={() => { setProviderFilter("all"); setProviderMenuOpen(false); }}
                    className={`flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left text-sm transition-colors ${providerFilter === "all" ? "bg-primary/10 text-primary" : "text-text-primary hover:bg-black/5 dark:hover:bg-white/10"}`}
                  >
                    <span className="material-symbols-outlined text-[22px]">apps</span>
                    <span className="font-medium">All providers</span>
                    {providerFilter === "all" && <span className="material-symbols-outlined ml-auto text-[20px]">check</span>}
                  </button>
                  <div className="my-1 h-px bg-black/10 dark:bg-white/10" />
                  <div className="max-h-72 overflow-y-auto pr-1">
                    {providerOptions.map((provider) => (
                      <button
                        key={provider}
                        type="button"
                        onClick={() => { setProviderFilter(provider); setProviderMenuOpen(false); }}
                        className={`flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left text-sm transition-colors ${providerFilter === provider ? "bg-primary/10 text-primary" : "text-text-primary hover:bg-black/5 dark:hover:bg-white/10"}`}
                      >
                        <ProviderIcon
                          src={`/providers/${provider}.png`}
                          alt={provider}
                          size={24}
                          className="size-6 rounded-md object-contain"
                          fallbackText={provider.slice(0, 2).toUpperCase()}
                        />
                        <span className="font-medium capitalize">{provider}</span>
                        {providerFilter === provider && <span className="material-symbols-outlined ml-auto text-[20px]">check</span>}
                      </button>
                    ))}
                  </div>
                </div>
              </>
            )}
          </div>
          <button
            type="button"
            onClick={() => setExpiringFirst((prev) => !prev)}
            className={`flex h-8 shrink-0 items-center gap-1 rounded-lg border px-2 text-xs transition-colors ${expiringFirst ? "border-amber-500/40 bg-amber-500/10 text-amber-500" : "border-black/10 text-text-primary hover:bg-black/5 dark:border-white/10 dark:hover:bg-white/5"}`}
            title="Sort accounts by earliest quota reset time"
          >
            <span className="material-symbols-outlined text-[14px]">hourglass_top</span>
            <span className="hidden sm:inline">Expiring first</span>
          </button>

          {/* Bulk: disable depleted */}
          <button
            type="button"
            onClick={handleDisableDepleted}
            disabled={bulkToggling}
            className="flex h-8 shrink-0 items-center gap-1 rounded-lg border border-red-500/30 px-2 text-xs text-red-500 transition-colors hover:bg-red-500/10 disabled:opacity-50"
            title="Disable connections with depleted quota (within current filter)"
          >
            <span className="material-symbols-outlined text-[14px]">block</span>
            <span className="hidden sm:inline">Turn off Empty</span>
          </button>

          {/* Bulk: enable available */}
          <button
            type="button"
            onClick={handleEnableAvailable}
            disabled={bulkToggling}
            className="flex h-8 shrink-0 items-center gap-1 rounded-lg border border-emerald-500/30 px-2 text-xs text-emerald-500 transition-colors hover:bg-emerald-500/10 disabled:opacity-50"
            title="Enable connections that still have quota (within current filter)"
          >
            <span className="material-symbols-outlined text-[14px]">check_circle</span>
            <span className="hidden sm:inline">Turn on Available</span>
          </button>

          {/* Auto-refresh toggle */}
          <button
            onClick={() => setAutoRefresh((prev) => !prev)}
            className="flex h-8 shrink-0 items-center gap-1 rounded-lg border border-black/10 px-2 text-xs transition-colors hover:bg-black/5 dark:border-white/10 dark:hover:bg-white/5"
            title={autoRefresh ? "Disable auto-refresh" : "Enable auto-refresh"}
          >
            <span
              className={`material-symbols-outlined text-[14px] ${
                autoRefresh ? "text-primary" : "text-text-muted"
              }`}
            >
              {autoRefresh ? "toggle_on" : "toggle_off"}
            </span>
            <span className="hidden text-text-primary sm:inline">Auto-refresh</span>
            {autoRefresh && (
              <span className="text-[10px] text-text-muted tabular-nums">({countdown}s)</span>
            )}
          </button>

          {/* Refresh all button */}
          <button
            type="button"
            onClick={refreshAll}
            disabled={refreshingAll}
            className="flex h-8 shrink-0 items-center gap-1 rounded-lg border border-black/10 px-2 text-xs text-text-primary transition-colors hover:bg-black/5 dark:border-white/10 dark:hover:bg-white/5 disabled:opacity-50"
            title="Refresh all"
          >
            <span className={`material-symbols-outlined text-[14px] ${refreshingAll ? "animate-spin" : ""}`}>refresh</span>
          </button>
        </div>
      </div>

      {/* Provider cards: 2 columns, compact */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
        {sortedConnections.map((conn) => {
          const quota = quotaData[conn.id];
          const isLoading = loading[conn.id];
          const error = errors[conn.id];

          // Use table layout for all providers
          const isInactive = conn.isActive === false;
          const isCodex = conn.provider === "codex";
          const resetCreditCount = getCodexResetCreditCount(quota);
          const isResettingLimit = resettingLimitId === conn.id;
          const rowBusy =
            deletingId === conn.id ||
            togglingId === conn.id ||
            isResettingLimit;

          return (
            <Card
              key={conn.id}
              padding="none"
              className={`min-w-0 ${isInactive ? "opacity-60" : ""}`}
            >
              <div className="px-3 py-2 border-b border-black/10 dark:border-white/10">
                <div className="flex items-center justify-between gap-2">
                  <div className="flex items-center gap-2 min-w-0">
                    <div className="w-8 h-8 shrink-0 rounded-md flex items-center justify-center overflow-hidden">
                      <ProviderIcon
                        src={`/providers/${conn.provider}.png`}
                        alt={conn.provider}
                        size={32}
                        className="object-contain"
                        fallbackText={
                          conn.provider?.slice(0, 2).toUpperCase() || "PR"
                        }
                      />
                    </div>
                    <div className="min-w-0">
                      <h3 className="text-sm font-semibold text-text-primary capitalize truncate">
                        {conn.provider}
                      </h3>
                      {(() => {
                        const label = getConnectionLabel(conn);
                        const secondary = getConnectionSecondaryLabel(conn);
                        if (!label && !secondary) return null;
                        return (
                          <>
                            {label && (
                              <p className="text-xs text-text-muted truncate">{label}</p>
                            )}
                            {secondary && (
                              <p className="text-[10px] text-text-muted/70 truncate">{secondary}</p>
                            )}
                          </>
                        );
                      })()}
                    </div>
                  </div>

                  <div className="flex items-center gap-1 shrink-0">
                    {AUTO_PING_SETTINGS_KEYS[conn.provider as keyof typeof AUTO_PING_SETTINGS_KEYS] && conn.authType === "oauth" && (
                      <Tooltip text={autoPingTooltips[conn.provider] || "Auto-ping warmup"}>
                        <button
                          type="button"
                          onClick={() => toggleAutoPing(conn.id, conn.provider, !(autoPingMaps[conn.provider]?.[conn.id] === true))}
                          className={`flex h-8 w-8 items-center justify-center rounded-lg transition-colors hover:bg-black/5 dark:hover:bg-white/5 ${autoPingMaps[conn.provider]?.[conn.id] === true ? "text-primary" : "text-text-muted"}`}
                          title="Toggle auto-ping warmup"
                        >
                          <span className="material-symbols-outlined text-[18px]">bolt</span>
                        </button>
                      </Tooltip>
                    )}
                    {isCodex && (
                      <>
                        <Tooltip
                          text={
                            resetCreditCount > 0
                              ? `Use one Codex reset credit. Available: ${resetCreditCount}`
                              : "No Codex reset credits available"
                          }
                        >
                          <button
                            type="button"
                            onClick={() =>
                              setResetConfirmState({
                                connection: conn,
                                resetCreditCount,
                              })
                            }
                            disabled={
                              resetCreditCount <= 0 || isLoading || rowBusy
                            }
                            aria-label={
                              resetCreditCount > 0
                                ? `Use one Codex reset credit. ${resetCreditCount} available.`
                                : "No Codex reset credits available"
                            }
                            className={`flex h-8 min-w-10 items-center justify-center gap-1 rounded-lg border px-2 text-[11px] font-medium tabular-nums transition-colors disabled:cursor-not-allowed disabled:opacity-60 ${
                              resetCreditCount > 0
                                ? "border-primary/30 bg-primary/5 text-primary hover:bg-primary/10"
                                : "border-black/10 bg-black/[0.02] text-text-muted dark:border-white/10 dark:bg-white/[0.03]"
                            }`}
                          >
                            <span
                              className={`material-symbols-outlined text-[15px] ${isResettingLimit ? "animate-spin" : ""}`}
                            >
                              {isResettingLimit
                                ? "progress_activity"
                                : "restart_alt"}
                            </span>
                            <span>{resetCreditCount}</span>
                          </button>
                        </Tooltip>
                        <Tooltip text="View Codex reset credit expiry">
                          <button
                            type="button"
                            onClick={() => handleViewCodexResetCredits(conn)}
                            disabled={isLoading || rowBusy}
                            aria-label="View Codex reset credit expiry"
                            className="flex h-8 w-8 items-center justify-center rounded-lg border border-black/10 text-text-muted transition-colors hover:bg-black/5 hover:text-primary disabled:cursor-not-allowed disabled:opacity-50 dark:border-white/10 dark:hover:bg-white/5"
                          >
                            <span className="material-symbols-outlined text-[17px]">
                              schedule
                            </span>
                          </button>
                        </Tooltip>
                      </>
                    )}
                    <button
                      type="button"
                      onClick={() => refreshProvider(conn.id, conn.provider)}
                      disabled={isLoading || rowBusy}
                      className="p-1.5 rounded-lg hover:bg-black/5 dark:hover:bg-white/5 transition-colors disabled:opacity-50"
                      title="Refresh quota"
                    >
                      <span
                        className={`material-symbols-outlined text-[18px] text-text-muted ${isLoading ? "animate-spin" : ""}`}
                      >
                        refresh
                      </span>
                    </button>
                    <button
                      type="button"
                      onClick={() => {
                        setSelectedConnection(conn);
                        setShowEditModal(true);
                      }}
                      disabled={rowBusy}
                      className="p-1.5 rounded-lg hover:bg-black/5 dark:hover:bg-white/5 text-text-muted hover:text-primary transition-colors disabled:opacity-50"
                      title="Edit connection"
                    >
                      <span className="material-symbols-outlined text-[18px]">
                        edit
                      </span>
                    </button>
                    <button
                      type="button"
                      onClick={() => handleDeleteConnection(conn.id)}
                      disabled={rowBusy}
                      className="p-1.5 rounded-lg hover:bg-red-500/10 text-red-500 transition-colors disabled:opacity-50"
                      title="Delete connection"
                    >
                      <span
                        className={`material-symbols-outlined text-[18px] ${deletingId === conn.id ? "animate-pulse" : ""}`}
                      >
                        delete
                      </span>
                    </button>
                    <div
                      className="inline-flex items-center pl-0.5"
                      title={
                        (conn.isActive ?? true)
                          ? "Disable connection"
                          : "Enable connection"
                      }
                    >
                      <Toggle
                        size="sm"
                        checked={conn.isActive ?? true}
                        disabled={rowBusy}
                        onChange={(nextActive: boolean) =>
                          handleToggleConnectionActive(conn.id, nextActive)
                        }
                      />
                    </div>
                  </div>
                </div>
              </div>

              <div className="px-2 py-1.5">
                {isLoading ? (
                  <div className="text-center py-5 text-text-muted">
                    <span className="material-symbols-outlined text-[28px] animate-spin">
                      progress_activity
                    </span>
                  </div>
                ) : error ? (
                  <div className="text-center py-5">
                    <span className="material-symbols-outlined text-[28px] text-red-500">
                      error
                    </span>
                    <p className="mt-1.5 text-xs text-text-muted">{error}</p>
                  </div>
                ) : quota?.message ? (
                  <div className="text-center py-5">
                    <p className="text-xs text-text-muted">{quota.message}</p>
                  </div>
                ) : (
                  <QuotaTable quotas={quota?.quotas} compact />
                )}
              </div>
            </Card>
          );
        })}
      </div>

      <EditConnectionModal
        isOpen={showEditModal}
        connection={selectedConnection}
        proxyPools={proxyPools}
        onSave={handleUpdateConnection}
        onClose={() => {
          setShowEditModal(false);
          setSelectedConnection(null);
        }}
      />

      <ConfirmModal
        isOpen={!!deleteConfirmId}
        onClose={() => deletingId ? undefined : setDeleteConfirmId(null)}
        onConfirm={confirmDeleteConnection}
        title="Delete connection"
        message="Are you sure you want to delete this connection? This action cannot be undone."
        confirmText="Delete"
        variant="danger"
        loading={deletingId !== null}
      />

      <ConfirmModal
        isOpen={Boolean(resetConfirmState)}
        onClose={() => {
          if (!resettingLimitId) setResetConfirmState(null);
        }}
        onConfirm={async () => {
          const connection = resetConfirmState?.connection;
          if (!connection) return;
          await handleResetCodexLimit(connection.id, connection.provider);
          setResetConfirmState(null);
        }}
        title="Reset Codex limit?"
        message={`Use 1 Codex reset credit for ${
          getConnectionLabel(resetConfirmState?.connection || { id: "", provider: "codex" }) ||
          "this account"
        }. This cannot be undone. Remaining credits: ${
          resetConfirmState?.resetCreditCount ?? 0
        }.`}
        confirmText="Reset limit"
        cancelText="Cancel"
        variant="danger"
        loading={Boolean(resettingLimitId)}
      />

      {resetCreditsState && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 px-4 backdrop-blur-sm">
          <div className="w-full max-w-2xl overflow-hidden rounded-2xl border border-black/15 bg-white shadow-2xl ring-1 ring-black/10 dark:border-white/15 dark:bg-neutral-950 dark:ring-white/10">
            <div className="flex items-start justify-between gap-3 border-b border-black/10 bg-black/[0.03] px-4 py-3 dark:border-white/10 dark:bg-white/[0.04]">
              <div className="min-w-0">
                <h3 className="text-base font-semibold text-text-primary">
                  Codex Reset Credit Expiry
                </h3>
                <p className="mt-0.5 truncate text-xs text-text-muted">
                  {getConnectionLabel(resetCreditsState.connection) ||
                    "Codex account"}
                </p>
              </div>
              <button
                type="button"
                onClick={() => setResetCreditsState(null)}
                className="flex h-8 w-8 items-center justify-center rounded-lg text-text-muted transition-colors hover:bg-black/5 hover:text-text-primary dark:hover:bg-white/5"
                aria-label="Close reset credit expiry modal"
              >
                <span className="material-symbols-outlined text-[18px]">
                  close
                </span>
              </button>
            </div>

            <div className="max-h-[70vh] overflow-auto bg-white p-4 dark:bg-neutral-950">
              {resetCreditsState.loading ? (
                <div className="flex items-center justify-center gap-2 py-10 text-sm text-text-muted">
                  <span className="material-symbols-outlined animate-spin text-[20px]">
                    progress_activity
                  </span>
                  Loading reset credits...
                </div>
              ) : resetCreditsState.error ? (
                <div className="rounded-xl border border-red-500/20 bg-red-500/10 px-3 py-2 text-sm text-red-600 dark:text-red-300">
                  {resetCreditsState.error}
                </div>
              ) : resetCreditsState.data?.credits?.length ? (
                <div className="space-y-3">
                  <div className="flex items-center justify-between rounded-xl border border-black/10 bg-black/[0.02] px-3 py-2 text-xs text-text-muted dark:border-white/10 dark:bg-white/[0.03]">
                    <span>
                      {resetCreditsState.data.credits.length} reset credit
                      {resetCreditsState.data.credits.length === 1 ? "" : "s"}
                    </span>
                    <span>
                      {resetCreditsState.data.availableCount ?? 0} available
                    </span>
                  </div>
                  <div className="overflow-x-auto rounded-xl border border-black/10 dark:border-white/10">
                    <table className="w-full min-w-[560px] text-left text-sm">
                      <thead className="bg-black/[0.03] text-xs uppercase tracking-wide text-text-muted dark:bg-white/[0.04]">
                        <tr>
                          <th className="px-3 py-2 font-medium">Status</th>
                          <th className="px-3 py-2 font-medium">Granted At</th>
                          <th className="px-3 py-2 font-medium">Expires At</th>
                          <th className="px-3 py-2 font-medium">Remaining</th>
                        </tr>
                      </thead>
                      <tbody>
                        {resetCreditsState.data.credits.map((credit, index) => (
                          <tr
                            key={`${credit.status}-${credit.expiresAt || index}`}
                            className="border-t border-black/5 dark:border-white/5"
                          >
                            <td className="px-3 py-2">
                              <span className="rounded-full bg-primary/10 px-2 py-0.5 text-xs font-medium text-primary">
                                {credit.status || "unknown"}
                              </span>
                            </td>
                            <td className="px-3 py-2 text-text-muted">
                              {formatCreditDate(credit.grantedAt)}
                            </td>
                            <td className="px-3 py-2 text-text-primary">
                              {formatCreditDate(credit.expiresAt)}
                            </td>
                            <td className="px-3 py-2 font-medium text-text-primary">
                              {formatTimeRemaining(credit.expiresAt)}
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                </div>
              ) : (
                <div className="rounded-xl border border-black/10 bg-black/[0.02] px-3 py-8 text-center text-sm text-text-muted dark:border-white/10 dark:bg-white/[0.03]">
                  No reset credit details returned for this account.
                </div>
              )}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
