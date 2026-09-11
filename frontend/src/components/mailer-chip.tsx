/**
 * "via Thunderbird" chip for the message header — the sender's mail client,
 * resolved from the raw User-Agent / X-Mailer header. Renders nothing for
 * absent or unrecognized values. Brand glyphs are vendored single-path SVGs
 * from simpleicons.org (CC0), recolored via `currentColor` to follow the
 * theme; Microsoft marks were pulled from the catalog, so Outlook and the
 * rest fall back to Lucide glyphs.
 */

import { Mail, Terminal } from 'lucide-react';
import type { ComponentType } from 'react';

import { t } from '@/i18n';
import { matchMailer, type MailerId } from '@/lib/mailer';
import type { SupportedLocale } from '@/types';

type IconComponent = ComponentType<{ className?: string }>;

function brandIcon(path: string): IconComponent {
  return function BrandIcon({ className }: { className?: string }) {
    return (
      <svg viewBox="0 0 24 24" className={className}>
        <path d={path} fill="currentColor" />
      </svg>
    );
  };
}

const ThunderbirdIcon = brandIcon(
  'M9.948 4.444h-.005c-1.92.788-2.126 2.55-1.817 3.499v.02C9.236 7.18 10.658 6.76 12 6.76c3.26 0 5.902 2.156 5.902 4.815 0 2.66-2.643 4.816-5.902 4.816l-.083-.002c-.155-.006-.354-.013-.435.118-.096.156.116.397.238.536 1.274 1.441 3.123 1.622 3.608 1.67l.076.008c-4.281.414-9.304-2.32-9.306-7.076 0-1.12.414-2.073 1.075-2.83l-.005-.002h-.003C7.31 6.38 6.376 3.47 4.629 2.898c-.124-.04-.246.054-.262.183-.23 1.924-.727 2.59-1.264 3.31-.805 1.08-1.39 2.328-1.365 3.698a10.99 10.99 0 0 1-.705-1.91c-.024-.09-.17-.365-.333-.272-.13.072-.227.274-.296.485A12.137 12.137 0 0 0 0 11.489c0 6.536 5.475 12 12 12 6.627 0 12-5.372 12-12 0-2.526-.781-4.87-2.115-6.805l.167-.002c.518 0 1.024.045 1.51.129-.734-.816-1.724-1.475-2.877-1.904a8.54 8.54 0 0 1 2.494-.495c-1.426-1.166-3.508-1.9-5.827-1.9-3.355 0-6.648 1.29-7.404 3.93zm.682 9.166c-.87-.905-3.473-3.91-3.473-3.91l.202.01 4.075 3.042c.305.223.74.22 1.043-.004l3.996-3.034.212-.018s-2.518 2.935-3.483 3.9c-.964.968-1.703.919-2.572.014zm2.774-10.083s.055.625-.576.824c-.722.227-1.042-.38-1.042-.38s.09-.417.676-.61c.626-.206.942.166.942.166z',
);

const AppleIcon = brandIcon(
  'M12.152 6.896c-.948 0-2.415-1.078-3.96-1.04-2.04.027-3.91 1.183-4.961 3.014-2.117 3.675-.546 9.103 1.519 12.09 1.013 1.454 2.208 3.09 3.792 3.039 1.52-.065 2.09-.987 3.935-.987 1.831 0 2.35.987 3.96.948 1.637-.026 2.676-1.48 3.676-2.948 1.156-1.688 1.636-3.325 1.662-3.415-.039-.013-3.182-1.221-3.22-4.857-.026-3.04 2.48-4.494 2.597-4.559-1.429-2.09-3.623-2.324-4.39-2.376-2-.156-3.675 1.09-4.61 1.09zM15.53 3.83c.843-1.012 1.4-2.427 1.245-3.83-1.207.052-2.662.805-3.532 1.818-.78.896-1.454 2.338-1.273 3.714 1.338.104 2.715-.688 3.559-1.701',
);

const ICONS: Record<MailerId, IconComponent> = {
  thunderbird: ThunderbirdIcon,
  'apple-mail': AppleIcon,
  mutt: function MuttIcon({ className }: { className?: string }) {
    return <Terminal className={className} aria-hidden />;
  },
  outlook: Mail,
  foxmail: Mail,
  qqmail: Mail,
  netease: Mail,
  k9mail: Mail,
  fairemail: Mail,
  evolution: Mail,
  spike: Mail,
  mailspring: Mail,
  generic: Mail,
};

export function MailerChip({
  mailer,
  locale,
}: {
  /** Raw User-Agent / X-Mailer header value from the message payload. */
  mailer: string | null | undefined;
  locale: SupportedLocale;
}) {
  const match = matchMailer(mailer);
  if (!match) return null;
  const Icon = ICONS[match.id];
  return (
    <span
      className="inline-flex items-center gap-1 text-[11px] text-muted-foreground"
      title={mailer ?? undefined}
    >
      <span aria-hidden className="inline-flex">
        <Icon className="size-3 shrink-0" />
      </span>
      {t(locale, 'mail.viaMailer', { mailer: match.name })}
    </span>
  );
}
