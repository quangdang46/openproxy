// Codex CLI profile helpers — ported from 9router
// `src/app/(dashboard)/dashboard/cli-tools/components/codexConfig.js` (v0.5.112).
//
// Profiles are `~/.codex/<name>.config.toml` files invoked via `codex -p <name>`.
// Only the helpers the profiles UI needs are ported; the provider-table parser
// (`getCurrentCodexProviderSettings`) belongs to a different upstream change.

export interface CodexProfile {
  name: string;
  model: string;
  command: string;
}

/** Derive a safe profile alias from a model id, deduped against existing names. */
export function deriveProfileNameFromModel(
  modelId: string,
  existingNames: string[] = [],
): string {
  if (!modelId || typeof modelId !== "string") return "model";
  let base = "";
  const trimmed = modelId.trim();
  if (trimmed.includes("/")) {
    base = trimmed
      .split("/")[0]
      .toLowerCase()
      .replace(/[^a-z0-9_-]/g, "");
  } else {
    base = trimmed.toLowerCase().replace(/[^a-z0-9_-]/g, "");
  }
  base = base.replace(/^[-_]+|[-_]+$/g, "") || "model";
  if (base === "config") base = "model-config";

  let candidate = base;
  let counter = 2;
  while (existingNames.includes(candidate)) {
    candidate = `${base}-${counter}`;
    counter++;
  }
  return candidate;
}

export function buildCodexProfileToml({ name, model }: { name: string; model: string }): string {
  return `# codex -p ${name}\nmodel = "${model}"\nmodel_provider = "openproxy"\n`;
}

export function parseCodexProfileModel(content: string): string {
  if (typeof content !== "string") return "";
  const match = content.match(/^\s*model\s*=\s*(["'])([^"\n]+)\1/m);
  return match ? match[2] : "";
}
