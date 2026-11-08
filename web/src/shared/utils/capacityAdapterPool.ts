/**
 * Capacity-adapter ("Vision Adapter") pool helpers — the ordered model list
 * behind CombosPageClient's CapacityAdapterCap. Ported from 9router
 * dashboard/combos/page.js (commit 3e4323e): the picker can deselect a model
 * and every removal falls back to the default pool member when the list
 * would otherwise empty.
 */

/** 9router DEFAULT_FALLBACK_MODEL — kept in CombosPageClient; passed in here. */

/** The modal may hand back a Model object (`{value, name}`) or a bare string. */
export function resolvePoolModelValue(model: unknown): string {
  if (typeof model === "string") return model;
  const m = model as { value?: string; name?: string } | null | undefined;
  return m?.value || m?.name || "";
}

/** Append `model` to the ordered pool; duplicates and empty values no-op. */
export function addPoolModel(models: string[], model: unknown): string[] {
  const value = resolvePoolModelValue(model);
  if (!value || models.includes(value)) return models;
  return [...models, value];
}

/** Remove `value` from the pool; an emptied pool resets to `[fallback]`. */
export function deselectPoolModel(
  models: string[],
  model: unknown,
  fallback: string,
): string[] {
  const value = resolvePoolModelValue(model);
  const next = models.filter((m) => m !== value);
  return next.length === 0 ? [fallback] : next;
}

/** Remove the entry at `index`; an emptied pool resets to `[fallback]`. */
export function removePoolModelAt(
  models: string[],
  index: number,
  fallback: string,
): string[] {
  const next = models.filter((_, i) => i !== index);
  return next.length === 0 ? [fallback] : next;
}
