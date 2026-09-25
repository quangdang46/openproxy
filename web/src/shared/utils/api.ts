/**
 * API utility functions for making HTTP requests
 */

import type { ApiResponse, ApiError } from "@/types";

const DEFAULT_HEADERS: Record<string, string> = {
  "Content-Type": "application/json",
};

interface FetchOptions extends RequestInit {
  headers?: Record<string, string>;
}

/**
 * Make a GET request
 * @param url - API endpoint
 * @param options - Fetch options
 * @returns Promise with response data
 */
export async function get<T = unknown>(
  url: string,
  options: FetchOptions = {}
): Promise<T> {
  const response = await fetch(url, {
    method: "GET",
    headers: { ...DEFAULT_HEADERS, ...options.headers },
    ...options,
  });
  return handleResponse<T>(response);
}

/**
 * Make a POST request
 * @param url - API endpoint
 * @param data - Request body
 * @param options - Fetch options
 * @returns Promise with response data
 */
export async function post<T = unknown>(
  url: string,
  data: unknown,
  options: FetchOptions = {}
): Promise<T> {
  const response = await fetch(url, {
    method: "POST",
    headers: { ...DEFAULT_HEADERS, ...options.headers },
    body: JSON.stringify(data),
    ...options,
  });
  return handleResponse<T>(response);
}

/**
 * Make a PUT request
 * @param url - API endpoint
 * @param data - Request body
 * @param options - Fetch options
 * @returns Promise with response data
 */
export async function put<T = unknown>(
  url: string,
  data: unknown,
  options: FetchOptions = {}
): Promise<T> {
  const response = await fetch(url, {
    method: "PUT",
    headers: { ...DEFAULT_HEADERS, ...options.headers },
    body: JSON.stringify(data),
    ...options,
  });
  return handleResponse<T>(response);
}

/**
 * Make a DELETE request
 * @param url - API endpoint
 * @param options - Fetch options
 * @returns Promise with response data
 */
export async function del<T = unknown>(
  url: string,
  options: FetchOptions = {}
): Promise<T> {
  const response = await fetch(url, {
    method: "DELETE",
    headers: { ...DEFAULT_HEADERS, ...options.headers },
    ...options,
  });
  return handleResponse<T>(response);
}

/**
 * Handle API response
 * @param response - Fetch response
 * @returns Promise with response data
 */
async function handleResponse<T>(response: Response): Promise<T> {
  const data = await response.json();

  if (!response.ok) {
    const error = new Error(data.error || "An error occurred") as Error & {
      status?: number;
      data?: unknown;
    };
    error.status = response.status;
    error.data = data;
    throw error;
  }

  return data as T;
}

const api = { get, post, put, del };
export default api;

/**
 * Fetch `/api/settings`, refusing to fabricate a value on failure.
 *
 * Settings are read-modify-written: a caller fetches the whole settings object,
 * changes one key, and PATCHes the result back. Degrading a failed read to `{}`
 * therefore makes the caller believe every other provider has no override and
 * PATCH that emptiness over the top — silently deleting settings it never read.
 * A toggle click is enough to trigger it, and the response is a normal 200.
 *
 * Throws instead, so the caller's existing error path runs and nothing is
 * written. Treat the absence of an override as an empty map only when the read
 * genuinely succeeded.
 */
export async function fetchSettingsStrict(): Promise<Record<string, unknown>> {
  const res = await fetch("/api/settings", { cache: "no-store" });
  if (!res.ok) {
    throw new Error(`Failed to load settings (HTTP ${res.status})`);
  }
  return (await res.json()) as Record<string, unknown>;
}
