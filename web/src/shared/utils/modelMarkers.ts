// Model context markers (9router open-sse/utils/modelMarkers.js parity).
//
// Claude Code appends a bracketed context marker to the model name when the
// 1M-context beta is toggled on: `claude-opus-5` becomes `claude-opus-5[1m]`.
// The marker is a client-side annotation, not part of any model id.

const CONTEXT_MARKER = /\[1m\]$/i;

/** Returns { model, contextMarker } — contextMarker is null when there is none. */
export function stripModelContextMarker(modelStr: unknown): {
  model: unknown;
  contextMarker: string | null;
} {
  if (typeof modelStr !== "string") return { model: modelStr, contextMarker: null };
  const trimmed = modelStr.trim();
  const match = trimmed.match(CONTEXT_MARKER);
  if (!match) return { model: modelStr, contextMarker: null };
  return { model: trimmed.slice(0, -match[0].length), contextMarker: match[0].slice(1, -1).toLowerCase() };
}
