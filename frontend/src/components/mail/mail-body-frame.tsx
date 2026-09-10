/**
 * Sandboxed email body renderer.
 *
 * HTML mail is authored against full CSS — `<style>` blocks, class selectors,
 * media queries — which the in-page renderer had to strip (global CSS leaks,
 * Tailwind piggyback). Rendering inside a sandboxed iframe restores that CSS
 * safely: the frame document shares nothing with the app and scripts cannot
 * run (`allow-scripts` is deliberately absent; `allow-same-origin` lets the
 * parent auto-size the frame and scan for tracking pixels).
 *
 * The frame is a paper-white sheet in both app themes: message HTML expects
 * a light canvas unless the sender ships their own dark media queries.
 */

import { useEffect, useMemo, useRef } from 'react';

import { t } from '@/i18n';
import { buildMailBodyDocument } from '@/lib/mail-body-document';
import { useUIStore } from '@/stores/ui';

/** Tracking-pixel heuristic: fully-loaded image no larger than 4×4 CSS px. */
function isPixelLike(img: HTMLImageElement): boolean {
  return img.complete && img.naturalWidth > 0 && img.naturalWidth <= 4 && img.naturalHeight <= 4;
}

export function MailBodyFrame({
  html,
  onTrackingPixel,
}: {
  /** Sanitized via sanitizeEmailHtmlForFrame; cid: images already resolved. */
  html: string;
  onTrackingPixel?: () => void;
}) {
  const locale = useUIStore((s) => s.locale);
  const frameRef = useRef<HTMLIFrameElement>(null);
  const doc = useMemo(() => buildMailBodyDocument(html), [html]);

  useEffect(() => {
    const frame = frameRef.current;
    if (!frame) return;
    let observer: ResizeObserver | null = null;
    let imgListenerDoc: Document | null = null;

    const onImgLoad = (ev: Event) => {
      const target = ev.target;
      if (target instanceof HTMLImageElement && isPixelLike(target)) onTrackingPixel?.();
    };

    const onLoad = () => {
      const inner = frame.contentDocument;
      if (!inner) return;
      const measure = () => {
        frame.style.height = `${inner.documentElement.scrollHeight}px`;
      };
      measure();
      if (inner.body) {
        observer = new ResizeObserver(measure);
        observer.observe(inner.body);
      }
      inner.querySelectorAll('img').forEach((img) => {
        if (isPixelLike(img)) onTrackingPixel?.();
      });
      inner.addEventListener('load', onImgLoad, true);
      imgListenerDoc = inner;
    };

    frame.addEventListener('load', onLoad);
    return () => {
      frame.removeEventListener('load', onLoad);
      observer?.disconnect();
      imgListenerDoc?.removeEventListener('load', onImgLoad, true);
    };
  }, [doc, onTrackingPixel]);

  return (
    <iframe
      ref={frameRef}
      title={t(locale, 'mail.messageBody')}
      // No allow-scripts: email JS can never run. allow-same-origin lets the
      // parent size the frame; popups escape so links open as normal tabs.
      sandbox="allow-same-origin allow-popups allow-popups-to-escape-sandbox"
      srcDoc={doc}
      className="block w-full border-0 bg-white"
      style={{ overflow: 'hidden' }}
    />
  );
}
