"use client";

import { useState, useEffect } from "react";
import { CardSkeleton } from "@/shared/components";
import { CLI_TOOLS, MITM_TOOLS } from "@/shared/constants/cliTools";
import { MitmLinkCard } from "./cli-tools";
import ToolSummaryCard from "./cli-tools/ToolSummaryCard";

const STATUS_ENDPOINTS: Record<string, string> = {
  claude: "/api/cli-tools/claude-settings",
  cline: "/api/cli-tools/cline-settings",
  kilo: "/api/cli-tools/kilo-settings",
  codex: "/api/cli-tools/codex-settings",
  opencode: "/api/cli-tools/opencode-settings",
  droid: "/api/cli-tools/droid-settings",
  openclaw: "/api/cli-tools/openclaw-settings",
  hermes: "/api/cli-tools/hermes-settings",
  cowork: "/api/cli-tools/cowork-settings",
  "deepseek-tui": "/api/cli-tools/deepseek-tui-settings",
  jcode: "/api/cli-tools/jcode-settings",
};

interface CLIToolsPageClientProps {
  machineId?: string;
}

/**
 * CLI Tools index — compact summary cards that navigate to /cli-tools/[toolId].
 * Full configuration cards live in ToolDetailClient (9router parity).
 */
export default function CLIToolsPageClient(_props: CLIToolsPageClientProps) {
  const [loading, setLoading] = useState<boolean>(true);
  const [toolStatuses, setToolStatuses] = useState<Record<string, any>>({});

  useEffect(() => {
    fetchAllStatuses().finally(() => setLoading(false));
  }, []);

  const fetchAllStatuses = async () => {
    try {
      // Prefer the aggregation endpoint; fall back to per-tool parallel fetches.
      const aggRes = await fetch("/api/cli-tools/all-statuses").catch(() => null);
      if (aggRes && aggRes.ok) {
        const data = await aggRes.json();
        if (data && typeof data === "object" && !("error" in data)) {
          setToolStatuses(data);
          return;
        }
      }
    } catch {
      // fall through to per-tool
    }

    try {
      const entries = await Promise.all(
        Object.entries(STATUS_ENDPOINTS).map(async ([toolId, url]) => {
          try {
            const res = await fetch(url);
            const data = await res.json();
            return [toolId, data];
          } catch {
            return [toolId, null];
          }
        }),
      );
      setToolStatuses(Object.fromEntries(entries));
    } catch (error) {
      console.log("Error fetching tool statuses:", error);
    }
  };

  if (loading) {
    return (
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 sm:gap-4 lg:grid-cols-3">
        <CardSkeleton />
        <CardSkeleton />
        <CardSkeleton />
        <CardSkeleton />
        <CardSkeleton />
        <CardSkeleton />
      </div>
    );
  }

  const regularTools = Object.entries(CLI_TOOLS);
  const mitmTools = Object.entries(MITM_TOOLS);

  return (
    <div className="mx-auto flex w-full max-w-5xl flex-col gap-6 px-1 sm:px-0">
      <div className="flex flex-col gap-1">
        <h1 className="text-xl font-semibold text-text-main sm:text-2xl">CLI Tools</h1>
        <p className="text-sm text-text-muted">
          Configure local coding tools to use your OpenProxy providers.
        </p>
      </div>

      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 sm:gap-4 lg:grid-cols-3">
        {regularTools.map(([toolId, tool]) => (
          <ToolSummaryCard
            key={toolId}
            toolId={toolId}
            tool={tool as any}
            status={toolStatuses[toolId]}
          />
        ))}
      </div>

      <div className="flex flex-col gap-3 sm:gap-4">
        <div className="flex items-center gap-2 px-1">
          <span className="material-symbols-outlined text-[18px] text-primary">security</span>
          <h2 className="text-sm font-semibold text-text-main">MITM Tools</h2>
        </div>
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 sm:gap-4 lg:grid-cols-3">
          {mitmTools.map(([toolId, tool]) => (
            <MitmLinkCard key={toolId} tool={tool} />
          ))}
        </div>
      </div>
    </div>
  );
}
