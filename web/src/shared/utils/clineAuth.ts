import pkg from "../../../package.json" with { type: "json" };

const APP_VERSION = pkg.version || "0.0.0";

export function getClineAccessToken(token: string | null | undefined): string {
  if (typeof token !== "string") return "";
  const trimmed = token.trim();
  if (!trimmed) return "";
  if (trimmed.toLowerCase().startsWith("workos:")) return trimmed;
  // 9router parity (open-sse/shared/clineAuth.js v0.5.75, commit f6e7cabe):
  // only WorkOS JWTs (eyJ…) get the workos: prefix. ClinePass API keys
  // (e.g. clp_…) are sent verbatim — prefixing them yields HTTP 401.
  const isWorkOsJwt = /^eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+/.test(trimmed);
  return isWorkOsJwt ? `workos:${trimmed}` : trimmed;
}

export function getClineAuthorizationHeader(token: string | null | undefined): string {
  const accessToken = getClineAccessToken(token);
  return accessToken ? `Bearer ${accessToken}` : "";
}

export function buildClineHeaders(token: string | null | undefined, extraHeaders: Record<string, string> = {}): Record<string, string> {
  const authorization = getClineAuthorizationHeader(token);
  const headers: Record<string, string> = {
    "HTTP-Referer": "https://cline.bot",
    "X-Title": "Cline",
    "User-Agent": `OpenProxy/${APP_VERSION}`,
    "X-PLATFORM": process.platform || "unknown",
    "X-PLATFORM-VERSION": process.version || "unknown",
    "X-CLIENT-TYPE": "openproxy",
    "X-CLIENT-VERSION": APP_VERSION,
    "X-CORE-VERSION": APP_VERSION,
    "X-IS-MULTIROOT": "false",
    ...extraHeaders,
  };

  if (authorization) {
    headers.Authorization = authorization;
  }

  return headers;
}
