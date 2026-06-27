"use client";

import { Card, Badge } from "@/shared/components";
import { useCopyToClipboard } from "@/shared/hooks/useCopyToClipboard";
import {
  SKILLS,
  SKILLS_REPO_URL,
  getSkillRawUrl,
  getSkillBlobUrl,
  type Skill,
} from "@/shared/constants/skills";

function CopyButton({ value, label = "Copy link" }: { value: string; label?: string }) {
  const { copied, copy } = useCopyToClipboard(2000);
  return (
    <button
      onClick={() => copy(value)}
      className="px-2 py-1 rounded-lg bg-surface-soft text-ink text-[11px] font-medium hover:bg-surface-card transition-colors cursor-pointer shrink-0 inline-flex items-center gap-1"
      title={value}
    >
      <span className="material-symbols-outlined text-[12px]">
        {copied ? "check" : "content_copy"}
      </span>
      {copied ? "Copied!" : label}
    </button>
  );
}

function SkillRow({ skill }: { skill: Skill }) {
  const url = getSkillRawUrl(skill.id);
  return (
    <div
      className={`flex items-start gap-3 p-4 rounded-xl border transition-colors ${
        skill.isEntry
          ? "border-brand-coral/40 bg-brand-coral/[0.04]"
          : "border-hairline bg-surface-card hover:bg-surface-soft"
      }`}
    >
      <div
        className={`size-9 rounded-lg flex items-center justify-center shrink-0 ${
          skill.isEntry
            ? "bg-brand-coral text-on-primary"
            : "bg-brand-coral/10 text-brand-coral"
        }`}
      >
        <span className="material-symbols-outlined text-[18px]">{skill.icon}</span>
      </div>

      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2 flex-wrap">
          <h3 className="font-semibold text-sm text-ink">{skill.name}</h3>
          {skill.isEntry && (
            <Badge variant="primary" size="sm">START HERE</Badge>
          )}
          {skill.endpoint && (
            <Badge variant="code" size="sm">
              <code className="text-[10px]">{skill.endpoint}</code>
            </Badge>
          )}
        </div>
        <p className="text-xs text-body mt-0.5">{skill.description}</p>
        <a
          href={getSkillBlobUrl(skill.id)}
          target="_blank"
          rel="noreferrer"
          className="text-[11px] text-body hover:text-brand-coral mt-1 inline-flex items-center gap-1 break-all"
        >
          {url}
          <span className="material-symbols-outlined text-[12px]">open_in_new</span>
        </a>
      </div>

      <CopyButton value={url} />
    </div>
  );
}

export default function SkillsPageClient() {
  return (
    <div className="max-w-4xl mx-auto space-y-6 pb-8">
      <Card padding="md">
        <div className="text-xs text-body mb-2">Paste this to your AI:</div>
        <div className="px-3 py-2 rounded-lg bg-surface-soft font-mono text-[12px] text-ink">
          Read this skill and use it: {getSkillRawUrl("openproxy")}
        </div>
      </Card>

      <div className="space-y-2">
        {SKILLS.map((skill) => (
          <SkillRow key={skill.id} skill={skill} />
        ))}
      </div>

      <Card padding="md">
        <div className="flex items-center justify-between gap-3 flex-wrap">
          <div>
            <h2 className="text-sm font-semibold text-ink">More on GitHub</h2>
            <p className="text-xs text-body mt-0.5">
              Browse source, README, and examples.
            </p>
          </div>
          <a
            href={`${SKILLS_REPO_URL}/tree/main/.agents/skills`}
            target="_blank"
            rel="noreferrer"
            className="text-sm text-brand-coral hover:underline inline-flex items-center gap-1"
          >
            <span className="material-symbols-outlined text-[16px]">open_in_new</span>
            View on GitHub
          </a>
        </div>
      </Card>
    </div>
  );
}
