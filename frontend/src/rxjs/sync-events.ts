/**
 * RxJS stream for sync events.
 *
 * Subscribes to `GET /api/v1/sync/events` (SSE, bearer auth) and retries
 * with backoff. Role: ASYNC / RECOVERY only — no Zustand writes here.
 */

import { Observable, Subject, retry, timer } from 'rxjs';

import { apiStream } from '@/lib/api-client';
import type { SyncEvent } from '@/types';

export const syncEventSubject = new Subject<SyncEvent>();

export const syncEvents$: Observable<SyncEvent> = syncEventSubject.asObservable();

const KNOWN_TYPES = new Set<SyncEvent['type']>([
  'sync_started',
  'folder_progress',
  'folder_complete',
  'incremental_complete',
  'sync_error',
  'sync_complete',
]);

export function parseSyncEvent(raw: string): SyncEvent | null {
  try {
    const value = JSON.parse(raw) as { type?: unknown };
    if (typeof value.type !== 'string' || !KNOWN_TYPES.has(value.type as SyncEvent['type'])) {
      return null;
    }
    return value as SyncEvent;
  } catch {
    return null;
  }
}

function framesFromBuffer(buffer: string): { frames: string[]; rest: string } {
  const frames: string[] = [];
  let rest = buffer;
  let idx = rest.indexOf('\n\n');
  while (idx >= 0) {
    frames.push(rest.slice(0, idx));
    rest = rest.slice(idx + 2);
    idx = rest.indexOf('\n\n');
  }
  return { frames, rest };
}

export function dataFromSseFrame(frame: string): string | null {
  const dataLines: string[] = [];
  for (const line of frame.split('\n')) {
    if (line.startsWith('data:')) {
      dataLines.push(line.slice(5).trimStart());
    }
  }
  if (dataLines.length === 0) return null;
  return dataLines.join('\n');
}

function sseObservable(): Observable<SyncEvent> {
  return new Observable<SyncEvent>((subscriber) => {
    const ac = new AbortController();

    const run = async () => {
      const res = await apiStream('/sync/events', ac.signal);
      const body = res.body;
      if (!body) {
        subscriber.error(new Error('SSE response had no body'));
        return;
      }
      const reader = body.getReader();
      const decoder = new TextDecoder();
      let buffer = '';
      while (!ac.signal.aborted) {
        const { done, value } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true }).replace(/\r\n/g, '\n');
        const split = framesFromBuffer(buffer);
        buffer = split.rest;
        for (const frame of split.frames) {
          const data = dataFromSseFrame(frame);
          if (!data) continue;
          const event = parseSyncEvent(data);
          if (event) subscriber.next(event);
        }
      }
      if (!ac.signal.aborted) {
        subscriber.complete();
      }
    };

    void run().catch((err: unknown) => {
      if (ac.signal.aborted) return;
      subscriber.error(err);
    });

    return () => ac.abort();
  }).pipe(
    retry({
      delay: (_error: unknown, retryCount: number) =>
        timer(Math.min(1000 * 2 ** retryCount, 60_000)),
    }),
  );
}

export function connectSyncSse(): Observable<SyncEvent> {
  return sseObservable();
}
