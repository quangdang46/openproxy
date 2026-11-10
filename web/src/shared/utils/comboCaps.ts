/**
 * Format a token count for the combo capability envelope — port of 9router
 * `fmtK` (combos/page.js). Unknown/zero renders as "?".
 */
export function fmtK(n?: number | null): string {
  if (!n) return "?";
  if (n >= 1_000_000) {
    const m = n / 1_000_000;
    return `${Number.isInteger(m) ? m : m.toFixed(1)}M`;
  }
  return `${Math.round(n / 1000)}k`;
}
