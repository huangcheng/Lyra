/**
 * Typed `/api/v1` client: bearer injection, JSON errors, session expiry.
 *
 * Unauthenticated calls set `{ auth: false }`. A 401 with a session-expiry
 * message clears the token and sends the user to `/login`; 401s that mean
 * "wrong password" / "bad TOTP" stay with the caller.
 */

import { useAuthStore } from '@/stores/auth';
import type { User } from '@/stores/auth';

export const API_V1 = '/api/v1';

export type ApiErrorCode = 'unauthorized' | 'http' | 'network';

export class ApiError extends Error {
  readonly status: number;
  readonly code: ApiErrorCode;

  constructor(status: number, code: ApiErrorCode, message: string) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
  }
}

export interface AuthMeResponse {
  id: string;
  username: string;
  display_name?: string;
  locale: string;
  totp_enabled: boolean;
  mark_read_policy: string;
}

export function userFromMe(me: AuthMeResponse): User {
  return {
    id: me.id,
    username: me.username,
    displayName: me.display_name,
    locale: me.locale,
    totpEnabled: me.totp_enabled,
    markReadPolicy: me.mark_read_policy,
  };
}

/** Session extractor: the backend's stable `code` field, with a message
 * regex as belt-and-braces for older/edge paths. */
export function isSessionExpiry(message: string, code: string | null): boolean {
  if (code === 'unauthorized') return true;
  return /expired session|missing authorization|invalid or expired session/i.test(message);
}

export interface ApiInit extends Omit<RequestInit, 'headers'> {
  headers?: HeadersInit;
  /** Default true. Login/bootstrap/status set this to false. */
  auth?: boolean;
}

function bearerToken(): string | null {
  return useAuthStore.getState().token ?? localStorage.getItem('lyra_token');
}

function apiUrl(path: string): string {
  if (path.startsWith('http://') || path.startsWith('https://')) return path;
  if (path.startsWith(API_V1)) return path;
  return `${API_V1}${path.startsWith('/') ? path : `/${path}`}`;
}

function clearSessionAndRedirect(): void {
  localStorage.removeItem('lyra_token');
  useAuthStore.getState().clearSession();
  if (window.location.pathname !== '/login') {
    window.location.assign('/login');
  }
}

async function errorBody(res: Response): Promise<{ message: string; code: string | null }> {
  const data = (await res.json().catch(() => ({}))) as { error?: unknown; code?: unknown };
  const message =
    typeof data.error === 'string' && data.error.length > 0 ? data.error : `HTTP ${res.status}`;
  const code = typeof data.code === 'string' ? data.code : null;
  return { message, code };
}

/**
 * JSON request against `/api/v1`. Empty 2xx bodies resolve to `undefined`.
 */
export async function api<T>(path: string, init: ApiInit = {}): Promise<T> {
  const { auth = true, headers: headerInit, ...rest } = init;
  const headers = new Headers(headerInit);
  if (rest.body != null && !(rest.body instanceof FormData) && !headers.has('Content-Type')) {
    headers.set('Content-Type', 'application/json');
  }
  if (auth) {
    const token = bearerToken();
    if (token) headers.set('Authorization', `Bearer ${token}`);
  }

  let res: Response;
  try {
    res = await fetch(apiUrl(path), { ...rest, headers });
  } catch {
    throw new ApiError(0, 'network', 'Network error');
  }

  if (res.status === 401) {
    const { message, code } = await errorBody(res);
    if (auth && isSessionExpiry(message, code)) {
      clearSessionAndRedirect();
    }
    throw new ApiError(401, 'unauthorized', message);
  }

  if (!res.ok) {
    const { message } = await errorBody(res);
    throw new ApiError(res.status, 'http', message);
  }

  if (res.status === 204) {
    return undefined as T;
  }
  const text = await res.text();
  if (!text) {
    return undefined as T;
  }
  return JSON.parse(text) as T;
}

/** Authenticated GET/POST SSE (EventSource cannot send Authorization). */
export async function apiStream(path: string, signal?: AbortSignal): Promise<Response> {
  const headers = new Headers({ Accept: 'text/event-stream' });
  const token = bearerToken();
  if (token) headers.set('Authorization', `Bearer ${token}`);

  let res: Response;
  try {
    res = await fetch(apiUrl(path), { headers, signal });
  } catch (err) {
    if (signal?.aborted) throw err;
    throw new ApiError(0, 'network', 'Network error');
  }

  if (res.status === 401) {
    const { message, code } = await errorBody(res);
    if (isSessionExpiry(message, code)) {
      clearSessionAndRedirect();
    }
    throw new ApiError(401, 'unauthorized', message);
  }
  if (!res.ok) {
    const { message } = await errorBody(res);
    throw new ApiError(res.status, 'http', message);
  }
  return res;
}
