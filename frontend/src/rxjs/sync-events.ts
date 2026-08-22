/**
 * RxJS stream for sync events.
 *
 * Subscribes to the backend SSE endpoint (/api/v1/sync/events) and provides
 * an observable stream of SyncEvents.
 *
 * Role: ASYNC / RECOVERY only. Long-lived subscriptions, retry logic,
 * backpressure. No data storage (→ Zustand), no flow logic (→ XState).
 *
 * In production this connects to a real SSE endpoint.
 * For now it exports the plumbing so the architecture is wired correctly.
 */

import { Observable, Subject, retry, timer } from 'rxjs';
import type { SyncEvent } from '../types';

/**
 * Subject that emits sync events.
 * In production, this would be fed by an EventSource (SSE) connection.
 * For now, components can subscribe and the stream is stubbed.
 */
export const syncEventSubject = new Subject<SyncEvent>();

/**
 * The main sync event observable with retry logic.
 *
 * In production, this would wrap an EventSource:
 *
 *   const sse$ = new Observable<SyncEvent>(subscriber => {
 *     const es = new EventSource('/api/v1/sync/events');
 *     es.onmessage = (e) => subscriber.next(JSON.parse(e.data));
 *     es.onerror = (e) => subscriber.error(e);
 *     return () => es.close();
 *   });
 *
 *   export const syncEvents$ = sse$.pipe(
 *     retry({ count: 3, delay: (_, retryCount) => timer(1000 * 2 ** retryCount) }),
 *   );
 */
export const syncEvents$: Observable<SyncEvent> = syncEventSubject.pipe(
  retry({
    count: 3,
    delay: (_error: unknown, retryCount: number) => timer(Math.min(1000 * 2 ** retryCount, 60_000)),
  }),
);
