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
