/**
 * `?provider=` deep-link helpers for the quota page — port of 9router
 * ProviderLimits (commit 65826d9, issue #4217). Astro has no
 * `useSearchParams`/`useRouter`, so the query string is read and rewritten
 * directly via `window.location` + `history.replaceState`.
 */

/** Read the `?provider=` deep-link param; returns "all" when absent/blank. */
export function providerFilterFromSearch(search: string): string {
  try {
    const value = new URLSearchParams(search).get("provider");
    return value && value.trim() ? value : "all";
  } catch {
    return "all";
  }
}

/**
 * Build `pathname?query` with the `provider` param set (or removed when the
 * filter is "all"), preserving every other query param. Mirrors upstream's
 * `router.replace` — a bookmarkable URL without a history entry.
 */
export function urlWithProviderParam(
  pathname: string,
  search: string,
  provider: string,
): string {
  const params = new URLSearchParams(search);
  if (!provider || provider === "all") {
    params.delete("provider");
  } else {
    params.set("provider", provider);
  }
  const qs = params.toString();
  return qs ? `${pathname}?${qs}` : pathname;
}
