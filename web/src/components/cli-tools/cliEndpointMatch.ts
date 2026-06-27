// Match a configured CLI base URL against all known endpoints (local/tunnel/tailscale/cloud)

interface EndpointOptions {
  tunnelPublicUrl?: string;
  tailscaleUrl?: string;
  cloudUrl?: string;
}

const stripTrailingSlash = (s: string): string => (s || "").replace(/\/+$/, "");

export function matchKnownEndpoint(currentUrl: string, opts: EndpointOptions = {}): boolean {
  if (!currentUrl) return false;
  const url = stripTrailingSlash(currentUrl);
  const { tunnelPublicUrl, tailscaleUrl, cloudUrl } = opts;
  if (/localhost|127\.0\.0\.1|0\.0\.0\.0/.test(url)) return true;
  if (tunnelPublicUrl && url.startsWith(stripTrailingSlash(tunnelPublicUrl))) return true;
  if (tailscaleUrl && url.startsWith(stripTrailingSlash(tailscaleUrl))) return true;
  if (cloudUrl && url.startsWith(stripTrailingSlash(cloudUrl))) return true;
  return false;
}
