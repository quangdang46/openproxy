"use client";

import { Card } from "@/shared/components";

interface Tool {
  name: string;
  description?: string;
  icon?: string;
  image?: string;
  color?: string;
}

interface Status {
  installed?: boolean;
  hasOpenProxy?: boolean;
}

interface ToolSummaryCardProps {
  toolId: string;
  tool: Tool;
  status?: Status | null;
}

// Derive simple connected/configured/not-installed status from API payload
function getStatus(status?: Status | null): { label: string; cls: string } {
  if (!status) return { label: "Unknown", cls: "bg-gray-500/10 text-gray-500" };
  if (!status.installed) return { label: "Not installed", cls: "bg-gray-500/10 text-gray-500" };
  if (status.hasOpenProxy) return { label: "Connected", cls: "bg-green-500/10 text-green-600 dark:text-green-400" };
  return { label: "Not configured", cls: "bg-yellow-500/10 text-yellow-600 dark:text-yellow-400" };
}

export default function ToolSummaryCard({ toolId, tool, status }: ToolSummaryCardProps): React.ReactNode {
  const s = getStatus(status);
  return (
    <a href={`/dashboard/cli-tools/${toolId}`} className="block">
      <Card padding="sm" className="h-full overflow-hidden hover:border-primary/50 transition-colors cursor-pointer">
        <div className="flex h-full flex-col gap-2">
          <div className="flex items-center gap-3">
            <div className="size-8 flex items-center justify-center shrink-0">
              {tool.image ? (
                <img src={tool.image} alt={tool.name} width={32} height={32} className="size-8 object-contain rounded-lg" sizes="32px" onError={(e) => { (e.target as HTMLImageElement).style.display = "none"; }} />
              ) : tool.icon ? (
                <span className="material-symbols-outlined text-[28px]" style={{ color: tool.color }}>{tool.icon}</span>
              ) : null}
            </div>
            <div className="min-w-0 flex-1">
              <h3 className="font-medium text-sm truncate">{tool.name}</h3>
              <span className={`inline-block mt-1 px-1.5 py-0.5 text-[10px] font-medium rounded-full ${s.cls}`}>{s.label}</span>
            </div>
            <span className="material-symbols-outlined text-text-muted text-[18px] shrink-0">chevron_right</span>
          </div>
        </div>
      </Card>
    </a>
  );
}
