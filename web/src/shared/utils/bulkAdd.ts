// Bulk-add API-key planner.
//
// Background: the backend upserts apikey connections BY NAME. A colliding
// name overwrites an existing key instead of inserting a new one. Bulk-add
// used to derive "<base> <lineIndex>" from the paste position, blind to
// existing names, so re-adding keys often silently replaced earlier ones.
//
// This planner gap-fills the smallest free "<base> <n>" against both existing
// connection names and names already assigned earlier in the same batch, so a
// generated name is never reused and the backend always inserts.
//
// Ported from 9router `src/shared/utils/bulkAdd.js`.
//
// Caveat: only numeric-suffix collision is handled. A user who manually
// types an exact existing non-numbered custom name (no index) will still hit
// the backend upsert — but bulk auto-naming always appends " <n>", so this
// path is unreachable from the bulk modal.

export interface ParsedBulkLine {
  baseName: string;
  apiKey: string;
  providerSpecificData?: { accountId: string };
}

export interface PlannedBulkEntry {
  name: string;
  apiKey: string;
  skipped: boolean;
  providerSpecificData?: { accountId: string };
}

/**
 * Parse one pipe-separated bulk line into { baseName, apiKey, providerSpecificData? }.
 */
function parseLine(
  line: string,
  opts: { isCloudflareAi?: boolean } = {},
): ParsedBulkLine | null {
  const { isCloudflareAi = false } = opts;
  const parts = line.split("|");

  if (isCloudflareAi && parts.length >= 3) {
    // name|apiKey|accountId (apiKey may itself contain pipes)
    const baseName = parts[0].trim();
    const apiKey = parts.slice(1, -1).join("|").trim();
    const accountId = parts[parts.length - 1].trim();
    return {
      baseName: baseName || "Key",
      apiKey,
      providerSpecificData: { accountId },
    };
  }

  if (parts.length >= 2) {
    // name|apiKey (apiKey may itself contain pipes)
    const baseName = parts[0].trim();
    const apiKey = parts.slice(1).join("|").trim();
    return { baseName: baseName || "Key", apiKey };
  }

  // apiKey only — auto-named "Key N"
  const apiKey = parts[0].trim();
  return { baseName: "Key", apiKey };
}

/**
 * Plan a bulk add: parse lines, assign collision-free "<base> <n>" names.
 *
 * @param lines raw paste lines
 * @param existingNames connection names already saved
 */
export function planBulkAdd(
  lines: string[],
  existingNames: string[] | null | undefined,
  opts: { isCloudflareAi?: boolean } = {},
): PlannedBulkEntry[] {
  const { isCloudflareAi = false } = opts;

  const safeExisting = Array.isArray(existingNames) ? existingNames : [];
  const used = new Set(
    safeExisting.map((n) => (typeof n === "string" ? n.toLowerCase() : "")),
  );

  const out: PlannedBulkEntry[] = [];
  for (const raw of lines) {
    const line = typeof raw === "string" ? raw.trim() : "";
    if (!line) continue;

    const parsed = parseLine(line, { isCloudflareAi });
    if (!parsed || !parsed.apiKey) continue;

    const base = parsed.baseName;

    // Gap-fill from 1: smallest free "<base> <n>" not in `used`.
    // O(batch * existing) — fine for bulk add (tens to low hundreds of keys).
    let idx = 1;
    let name: string;
    for (;;) {
      name = `${base} ${idx}`;
      if (!used.has(name.toLowerCase())) break;
      idx += 1;
    }
    used.add(name.toLowerCase());

    const entry: PlannedBulkEntry = { name, apiKey: parsed.apiKey, skipped: false };
    if (parsed.providerSpecificData)
      entry.providerSpecificData = parsed.providerSpecificData;
    out.push(entry);
  }
  return out;
}
