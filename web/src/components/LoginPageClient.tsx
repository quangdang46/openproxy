"use client";

import { useEffect, useMemo, useState } from "react";
import { Button, Card, Input } from "@/shared/components";

const DEFAULT_OIDC_LABEL = "Sign in with OIDC";
const DEFAULT_SAML_LABEL = "Sign in with SAML SSO";

type AuthMode = "password" | "oidc" | "sso" | "saml" | "both";

interface AuthStatus {
  requireLogin?: boolean;
  hasPassword?: boolean;
  authMode?: string;
  ssoType?: string;
  oidcConfigured?: boolean;
  oidcLoginLabel?: string;
  samlConfigured?: boolean;
  samlLoginLabel?: string;
  authenticated?: boolean;
}

function normalizeAuthMode(value: unknown): AuthMode {
  if (
    value === "oidc" ||
    value === "both" ||
    value === "password" ||
    value === "sso" ||
    value === "saml"
  )
    return value;
  return "password";
}

function extractRetryAfter(data: Record<string, unknown>, res: Response): number {
  const body =
    typeof data.retryAfter === "number"
      ? data.retryAfter
      : typeof data.retry_after_secs === "number"
        ? data.retry_after_secs
        : 0;
  if (body > 0) return Math.ceil(body);

  const header = res.headers.get("Retry-After");
  if (header) {
    const parsed = Number(header);
    if (Number.isFinite(parsed) && parsed > 0) return Math.ceil(parsed);
  }
  return 0;
}

export default function LoginPageClient() {
  const [password, setPassword] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [error, setError] = useState("");
  const [resetHint, setResetHint] = useState("");
  const [retryAfter, setRetryAfter] = useState(0);
  const [loading, setLoading] = useState(false);
  const [statusLoading, setStatusLoading] = useState(true);
  const [hasPassword, setHasPassword] = useState<boolean | null>(null);
  const [authMode, setAuthMode] = useState<AuthMode>("password");
  const [ssoType, setSsoType] = useState("oidc");
  const [oidcConfigured, setOidcConfigured] = useState(false);
  const [oidcLoginLabel, setOidcLoginLabel] = useState(DEFAULT_OIDC_LABEL);
  const [samlConfigured, setSamlConfigured] = useState(false);
  const [samlLoginLabel, setSamlLoginLabel] = useState(DEFAULT_SAML_LABEL);
  const [mustChange, setMustChange] = useState(false);

  // Countdown for rate-limit lockouts.
  useEffect(() => {
    if (retryAfter <= 0) return;
    const id = window.setInterval(() => {
      setRetryAfter((seconds) => (seconds > 0 ? seconds - 1 : 0));
    }, 1000);
    return () => window.clearInterval(id);
  }, [retryAfter]);

  // Bootstrap auth mode / redirect if login is not required.
  useEffect(() => {
    let cancelled = false;
    const controller = new AbortController();
    const timeoutId = window.setTimeout(() => controller.abort(), 5000);

    (async () => {
      try {
        const res = await fetch("/api/auth/status", { signal: controller.signal });
        if (!res.ok) {
          if (!cancelled) {
            setHasPassword(true);
            setStatusLoading(false);
          }
          return;
        }
        const data = (await res.json()) as AuthStatus;
        if (cancelled) return;

        if (data.requireLogin === false || data.authenticated === true) {
          window.location.assign("/dashboard");
          return;
        }

        setHasPassword(!!data.hasPassword);
        setAuthMode(normalizeAuthMode(data.authMode));
        setSsoType(
          typeof data.ssoType === "string" && data.ssoType ? data.ssoType : "oidc",
        );
        setOidcConfigured(data.oidcConfigured === true);
        setOidcLoginLabel(
          (data.oidcLoginLabel && data.oidcLoginLabel.trim()) || DEFAULT_OIDC_LABEL,
        );
        setSamlConfigured(data.samlConfigured === true);
        setSamlLoginLabel(
          (data.samlLoginLabel && data.samlLoginLabel.trim()) || DEFAULT_SAML_LABEL,
        );
      } catch {
        if (!cancelled) setHasPassword(true);
      } finally {
        window.clearTimeout(timeoutId);
        if (!cancelled) setStatusLoading(false);
      }
    })();

    return () => {
      cancelled = true;
      controller.abort();
      window.clearTimeout(timeoutId);
    };
  }, []);

  // Surface OIDC callback errors from the query string.
  useEffect(() => {
    try {
      const params = new URLSearchParams(window.location.search);
      const oidcError = params.get("error");
      if (oidcError) {
        setError(`SSO sign-in failed: ${oidcError}`);
      }
    } catch {
      /* ignore */
    }
  }, []);

  // 9router login/page.js (65197ad1): SSO dispatch by authMode + ssoType.
  const isSsoEnabled = ["sso", "oidc", "saml", "both"].includes(authMode);
  const activeSsoType = ssoType || (authMode === "saml" ? "saml" : "oidc");

  const samlAvailable = isSsoEnabled && activeSsoType === "saml" && samlConfigured;
  const oidcAvailable = isSsoEnabled && activeSsoType === "oidc" && oidcConfigured;
  const ssoAvailable = samlAvailable || oidcAvailable;
  const passwordAvailable =
    authMode === "password" || authMode === "both" || !ssoAvailable;

  const subtitle = useMemo(() => {
    if (mustChange) return "Choose a new password before continuing";
    if (samlAvailable) {
      return "Sign in with SAML 2.0 Single Sign-On";
    }
    if (oidcAvailable) {
      return "Sign in with your OIDC provider to access the dashboard";
    }
    if (authMode === "both" && ssoAvailable) {
      return `Sign in with password or ${activeSsoType === "saml" ? "SAML SSO" : "OIDC"}`;
    }
    return "Enter your password to access the dashboard";
  }, [authMode, mustChange, samlAvailable, oidcAvailable, ssoAvailable, activeSsoType]);

  const handleLogin = async (event: React.FormEvent) => {
    event.preventDefault();
    if (retryAfter > 0) return;

    setLoading(true);
    setError("");
    setResetHint("");

    try {
      const res = await fetch("/api/auth/login", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ password }),
      });

      let data: Record<string, unknown> = {};
      try {
        data = (await res.json()) as Record<string, unknown>;
      } catch {
        data = {};
      }

      if (res.ok && (data.success === true || data.authenticated === true)) {
        if (data.mustChangePassword === true) {
          setMustChange(true);
          setNewPassword("");
          setConfirmPassword("");
          return;
        }
        window.location.assign("/dashboard");
        return;
      }

      const message =
        typeof data.error === "string" && data.error
          ? data.error
          : "Invalid password. Please try again.";
      setError(message);
      if (typeof data.resetHint === "string" && data.resetHint) {
        setResetHint(data.resetHint);
      }
      const wait = extractRetryAfter(data, res);
      if (wait > 0) setRetryAfter(wait);
      setPassword("");
    } catch {
      setError("Connection failed. Is the server running?");
    } finally {
      setLoading(false);
    }
  };

  const handleSetNewPassword = async (event: React.FormEvent) => {
    event.preventDefault();
    setError("");

    if (newPassword.length < 8) {
      setError("New password must be at least 8 characters long");
      return;
    }
    if (newPassword !== confirmPassword) {
      setError("Passwords do not match");
      return;
    }

    setLoading(true);
    try {
      // Prefer the dedicated password endpoint (OpenProxy). Keep settings PATCH
      // as a soft fallback for older servers.
      let res = await fetch("/api/auth/password", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          currentPassword: password || undefined,
          newPassword,
        }),
      });

      if (res.status === 404 || res.status === 405) {
        res = await fetch("/api/settings", {
          method: "PATCH",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            currentPassword: password || undefined,
            newPassword,
          }),
        });
      }

      let data: Record<string, unknown> = {};
      try {
        data = (await res.json()) as Record<string, unknown>;
      } catch {
        data = {};
      }

      if (res.ok) {
        // First-time set keeps the session; rotation may invalidate it.
        if (data.sessionsInvalidated === true) {
          window.location.assign("/login");
        } else {
          window.location.assign("/dashboard");
        }
        return;
      }

      setError(
        typeof data.error === "string" && data.error
          ? data.error
          : "Failed to set password",
      );
    } catch {
      setError("An error occurred. Please try again.");
    } finally {
      setLoading(false);
    }
  };

  const handleOidcLogin = () => {
    window.location.href = "/api/auth/oidc/login";
  };

  const handleSamlLogin = () => {
    window.location.href = "/api/auth/saml/start";
  };

  if (statusLoading) {
    return (
      <div className="min-h-screen flex items-center justify-center bg-canvas px-4 py-12 relative overflow-hidden">
        <div className="landing-grid absolute inset-0 pointer-events-none -z-10" aria-hidden="true" />
        <div className="text-center relative z-10">
          <div className="inline-block h-8 w-8 animate-spin rounded-full border-2 border-brand-coral border-t-transparent" />
          <p className="mt-4 text-[14px] text-muted">Loading…</p>
        </div>
      </div>
    );
  }

  return (
    <div className="min-h-screen flex items-center justify-center bg-canvas px-4 py-12 relative overflow-hidden">
      <div className="landing-grid absolute inset-0 pointer-events-none -z-10" aria-hidden="true" />

      <div className="w-full max-w-md relative z-10">
        <div className="text-center mb-10">
          <div className="inline-flex items-center justify-center w-14 h-14 rounded-mini-md bg-surface-card border border-hairline mb-5">
            <svg
              width="28"
              height="28"
              viewBox="0 0 24 24"
              fill="currentColor"
              className="text-brand-coral"
              aria-hidden="true"
            >
              <path d="M11.0 2.2 L13.0 2.2 L13.4 10.0 L20.0 8.4 L20.5 10.3 L13.6 11.45 L20.5 13.7 L20.0 15.6 L13.4 14.0 L13.0 21.8 L11.0 21.8 L10.6 14.0 L4.0 15.6 L3.5 13.7 L10.4 11.45 L3.5 10.3 L4.0 8.4 L10.6 10.0 Z" />
            </svg>
          </div>
          <h1 className="font-serif font-normal text-[40px] leading-tight tracking-[-0.02em] text-ink mb-2">
            OpenProxy
          </h1>
          <p className="text-[15px] text-body">{subtitle}</p>
        </div>

        <Card padding="xl" radius="xl" tone="cream" className="shadow-none">
          {mustChange ? (
            <form onSubmit={handleSetNewPassword} className="flex flex-col gap-5">
              <p className="text-[13px] text-center text-amber-700 dark:text-amber-400 bg-amber-500/10 border border-amber-500/20 rounded-mini-md px-3 py-2">
                You signed in with the default password from a remote client. Set a new
                password before accessing the dashboard.
              </p>

              <Input
                type="password"
                label="New password"
                placeholder="At least 8 characters"
                value={newPassword}
                onChange={(e) => setNewPassword(e.target.value)}
                required
                autoFocus
                autoComplete="new-password"
              />
              <Input
                type="password"
                label="Confirm password"
                placeholder="Re-enter new password"
                value={confirmPassword}
                onChange={(e) => setConfirmPassword(e.target.value)}
                required
                autoComplete="new-password"
              />

              {error && (
                <p className="text-sm text-[color:var(--color-danger)] bg-[color:var(--color-danger)]/10 border border-[color:var(--color-danger)]/20 rounded-mini-md px-3 py-2">
                  {error}
                </p>
              )}

              <Button
                type="submit"
                variant="primary"
                fullWidth
                loading={loading}
                disabled={!newPassword || !confirmPassword}
              >
                Set password
              </Button>
            </form>
          ) : (
            <div className="flex flex-col gap-5">
              {samlAvailable && (
                <Button type="button" variant="primary" fullWidth onClick={handleSamlLogin}>
                  {samlLoginLabel}
                </Button>
              )}

              {oidcAvailable && (
                <Button type="button" variant="primary" fullWidth onClick={handleOidcLogin}>
                  {oidcLoginLabel}
                </Button>
              )}

              {ssoAvailable && passwordAvailable && (
                <div className="flex items-center gap-3">
                  <div className="h-px flex-1 bg-hairline" />
                  <span className="text-[12px] text-muted">or</span>
                  <div className="h-px flex-1 bg-hairline" />
                </div>
              )}

              {passwordAvailable ? (
                <form onSubmit={handleLogin} className="flex flex-col gap-5">
                  {isSsoEnabled && !ssoAvailable && (
                    <p className="text-[12px] text-center text-amber-700 dark:text-amber-400 bg-amber-500/10 border border-amber-500/20 rounded-mini-md px-3 py-2">
                      {activeSsoType === "saml" ? "SAML SSO" : "OIDC"} login is
                      enabled, but configuration is incomplete. Password login is
                      still available for recovery.
                    </p>
                  )}

                  {authMode === "both" && ssoAvailable && (
                    <p className="text-[12px] text-center text-muted">
                      Password and{" "}
                      {activeSsoType === "saml" ? "SAML SSO" : "OIDC"} login are
                      both enabled.
                    </p>
                  )}

                  <Input
                    type="password"
                    label="Password"
                    placeholder="Enter your password"
                    value={password}
                    onChange={(e) => setPassword(e.target.value)}
                    required
                    autoFocus={!ssoAvailable}
                    autoComplete="current-password"
                    disabled={retryAfter > 0}
                  />

                  {error && (
                    <p className="text-sm text-[color:var(--color-danger)] bg-[color:var(--color-danger)]/10 border border-[color:var(--color-danger)]/20 rounded-mini-md px-3 py-2">
                      {error}
                    </p>
                  )}

                  {retryAfter > 0 && (
                    <p className="text-[12px] text-center text-amber-700 dark:text-amber-400">
                      Locked. Retry in{" "}
                      <span className="font-mono font-medium">{retryAfter}s</span>.
                    </p>
                  )}

                  {resetHint && (
                    <p className="text-[12px] text-muted text-center">
                      Forgot password? Run{" "}
                      <code className="px-1.5 py-0.5 rounded bg-canvas border border-hairline-soft font-mono text-ink">
                        openproxy auth reset-password
                      </code>{" "}
                      on the host to restore the generated initial password.
                    </p>
                  )}

                  <Button
                    type="submit"
                    variant="primary"
                    fullWidth
                    loading={loading}
                    disabled={retryAfter > 0}
                  >
                    {retryAfter > 0 ? `Wait ${retryAfter}s` : "Sign In"}
                  </Button>

                  {hasPassword === false && (
                    <p className="text-[12px] text-center text-amber-700 dark:text-amber-400">
                      Security risk: no password set. You will be asked to set one when
                      logging in remotely.
                    </p>
                  )}
                </form>
              ) : (
                error && (
                  <p className="text-sm text-[color:var(--color-danger)] bg-[color:var(--color-danger)]/10 border border-[color:var(--color-danger)]/20 rounded-mini-md px-3 py-2">
                    {error}
                  </p>
                )
              )}
            </div>
          )}
        </Card>

        <p className="mt-8 text-center text-[12px] text-muted-soft">
          OpenProxy · local AI router for Claude Code, Codex, Cursor &amp; more
        </p>
      </div>
    </div>
  );
}
