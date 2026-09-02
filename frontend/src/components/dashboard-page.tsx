/**
 * Dashboard page: mail volume, top senders, and per-account unread.
 *
 * Data comes from `GET /api/v1/messages/stats`; per-account unread/messages
 * are derived from the folder counts already in the mail store. Storage has
 * no backend endpoint yet, so that KPI renders an honest empty state.
 *
 * Mirrors the v3 main-screen conventions: responsive single-column layout
 * under 1024px (same breakpoint as the mail three-pane), ink chart bars
 * (`--primary` is ink for the stamp / sparse solid actions), today's bar in the unread gold,
 * and locale-correct date letters (zh renders via date-fns zhCN).
 */

import { useEffect, useState } from 'react';
import { format, parseISO } from 'date-fns';
import { zhCN } from 'date-fns/locale';
import { ArrowLeft, BarChart3, Inbox, Users } from 'lucide-react';
import { Link } from '@tanstack/react-router';
import { EmptyState } from '@/components/empty-state';
import { SlimPageNav } from '@/components/slim-page-nav';
import { StampLogo } from '@/components/stamp-logo';
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { t } from '@/i18n';
import { useMediaQuery } from '@/lib/use-media-query';
import { useMailData } from '@/lib/use-mail-data';
import { fetchStats } from '@/lib/stats-api';
import { cn, getInitials } from '@/lib/utils';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';
import type { DailyVolume, StatsResponse } from '@/types';

const RANGES = [7, 30, 90] as const;

export function DashboardPage() {
  const locale = useUIStore((s) => s.locale);
  const accounts = useMailStore((s) => s.accounts);
  const folders = useMailStore((s) => s.folders);
  // Load accounts/folders even on a cold deep-link to /dashboard (the mail
  // page normally populates the store); also refreshes on sync completion.
  useMailData();
  const isMobile = useMediaQuery('(max-width: 1023px)');
  const [days, setDays] = useState<number>(30);
  const [stats, setStats] = useState<StatsResponse | null>(null);
  const [daily, setDaily] = useState<DailyVolume[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        setLoading(true);
        setError(null);
        const data = await fetchStats(days);
        if (!cancelled) {
          setStats(data);
          setDaily(fillDailySeries(data.daily, days));
        }
      } catch (err: unknown) {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [days]);

  const dateLocale = locale === 'zh' ? zhCN : undefined;

  // Per-account unread/message counts from folder counts already in the store.
  const accountStats = accounts.map((account) => {
    const accountFolders = Object.values(folders).filter((f) => f.accountId === account.id);
    return {
      account,
      unread: accountFolders.reduce((sum, f) => sum + f.unreadCount, 0),
      total: accountFolders.reduce((sum, f) => sum + f.totalCount, 0),
    };
  });
  const maxAccountTotal = Math.max(...accountStats.map((a) => a.total), 1);

  const rangeTabs = (
    <Tabs value={String(days)} onValueChange={(v) => setDays(Number(v))}>
      <TabsList className="h-8 rounded-lg bg-accent p-0.5 text-muted-foreground">
        {RANGES.map((r) => (
          <TabsTrigger
            key={r}
            value={String(r)}
            className="h-7 flex-none rounded-md px-3 text-sm font-medium shadow-none data-[state=active]:bg-card data-[state=active]:text-foreground"
          >
            {t(locale, `dash.range${r}`)}
          </TabsTrigger>
        ))}
      </TabsList>
    </Tabs>
  );

  if (isMobile) {
    return (
      <div className="flex h-full flex-col bg-background">
        <header className="flex flex-wrap items-center gap-x-3 gap-y-2 border-b border-border px-4 pb-4 pt-4">
          <Link
            to="/"
            className="flex items-center gap-1.5 text-[13px] text-muted-foreground hover:text-foreground"
          >
            <ArrowLeft size={16} /> {t(locale, 'nav.mail')}
          </Link>
          <div className="ml-auto">{rangeTabs}</div>
          <div className="flex w-full items-center gap-2.5">
            <StampLogo size={24} />
            <span className="font-brand text-base text-foreground">Lyra</span>
            <span className="text-[10.5px] font-semibold uppercase tracking-[0.8px] text-muted-foreground">
              {t(locale, 'dash.section')}
            </span>
          </div>
          <p className="w-full text-[12.5px] text-muted-foreground">{t(locale, 'dash.subtitle')}</p>
        </header>
        <main className="flex-1 overflow-auto">
          <DashboardBody
            /* state + data */
            loading={loading}
            error={error}
            stats={stats}
            daily={daily}
            accountStats={accountStats}
            maxAccountTotal={maxAccountTotal}
            /* presentation */
            locale={locale}
            days={days}
            dateLocale={dateLocale}
            mobile
          />
        </main>
      </div>
    );
  }

  return (
    <div className="flex h-svh">
      <SlimPageNav
        section={t(locale, 'dash.section')}
        items={[
          { key: 'overview', label: t(locale, 'dash.overview'), icon: BarChart3, active: true },
        ]}
      />
      <main className="flex-1 overflow-auto bg-background">
        {/* One centered measure for header + content: cards stop stretching
            into billboards on ultra-wide displays. */}
        <div className="mx-auto w-full max-w-5xl">
          <header className="flex flex-wrap items-center gap-x-4 gap-y-3 border-b border-border px-8 pb-5 pt-7">
            <div>
              <h1 className="font-display text-xl font-medium">{t(locale, 'dash.section')}</h1>
              <p className="text-[12.5px] text-muted-foreground">{t(locale, 'dash.subtitle')}</p>
            </div>
            <div className="flex-1" />
            {rangeTabs}
          </header>
          <DashboardBody
            loading={loading}
            error={error}
            stats={stats}
            daily={daily}
            accountStats={accountStats}
            maxAccountTotal={maxAccountTotal}
            locale={locale}
            days={days}
            dateLocale={dateLocale}
          />
        </div>
      </main>
    </div>
  );
}

interface BodyProps {
  loading: boolean;
  error: string | null;
  stats: StatsResponse | null;
  daily: DailyVolume[];
  accountStats: { account: { id: string; displayName: string }; unread: number; total: number }[];
  maxAccountTotal: number;
  locale: 'en' | 'zh';
  days: number;
  dateLocale: typeof zhCN | undefined;
  mobile?: boolean;
}

/** Content below the header; grid collapses to one/two columns on mobile. */
function DashboardBody({
  loading,
  error,
  stats,
  daily,
  accountStats,
  maxAccountTotal,
  locale,
  days,
  dateLocale,
  mobile = false,
}: BodyProps) {
  if (loading && !stats) {
    return (
      <p className={cn('pt-6 text-sm text-muted-foreground', mobile ? 'px-4' : 'px-8')}>
        {t(locale, 'common.loading')}
      </p>
    );
  }
  if (error) {
    return <EmptyState icon={BarChart3} title={t(locale, 'common.error')} hint={error} />;
  }
  if (!stats) return null;

  const maxReceived = Math.max(...daily.map((d) => d.received), 1);

  return (
    <>
      <div
        className={cn(
          'stagger-in grid gap-3 pt-6',
          mobile ? 'grid-cols-2 px-4' : 'grid-cols-4 gap-4 px-8',
        )}
      >
        <KpiCard
          label={t(locale, 'dash.unread')}
          value={stats.totals.unread}
          sub={t(locale, 'mail.folder.inbox')}
        />
        <KpiCard
          label={t(locale, 'dash.receivedToday')}
          value={daily.length > 0 ? daily[daily.length - 1].received : 0}
          sub={new Date().toLocaleDateString(locale === 'zh' ? 'zh-CN' : 'en-US', {
            month: 'short',
            day: 'numeric',
          })}
        />
        <KpiCard
          label={t(locale, 'dash.sentThisWeek')}
          value={stats.totals.sent}
          sub={t(locale, `dash.range${days}`)}
        />
        <KpiCard
          label={t(locale, 'dash.storage')}
          value="—"
          sub={t(locale, 'dash.storageUnavailable')}
        />
      </div>

      <section
        className={cn(
          'rounded-[10px] border border-border bg-card px-5 py-4',
          mobile ? 'mx-4 mt-3' : 'mx-8 mt-4',
        )}
      >
        <h2 className="text-[13.5px] font-semibold">{t(locale, 'dash.volume', { days })}</h2>
        <div className="mt-3 flex h-[140px] items-end gap-1.5">
          {daily.map((d, i) => (
            <div key={d.date} className="flex h-full flex-1 items-end">
              <div
                className={cn(
                  'w-full rounded-sm',
                  // Ink bars; today alone carries the unread gold.
                  i === daily.length - 1 ? 'bg-unread' : 'bg-foreground',
                )}
                style={{
                  height: `${Math.max((d.received / maxReceived) * 100, 1.5)}%`,
                }}
                title={`${d.date}: ${d.received}`}
              />
            </div>
          ))}
        </div>
        <div className="mt-1.5 flex gap-1.5">
          {daily.map((d) => (
            <div key={d.date} className="flex-1 text-center text-[10px] text-muted-foreground">
              {format(parseISO(d.date), 'EEEEE', { locale: dateLocale })}
            </div>
          ))}
        </div>
      </section>

      <div
        className={cn(
          'stagger-in grid gap-3 py-4',
          mobile ? 'grid-cols-1 px-4' : 'grid-cols-2 gap-4 px-8 py-4',
        )}
      >
        <section className="rounded-[10px] border border-border bg-card px-5 py-4">
          <h2 className="text-[13.5px] font-semibold">{t(locale, 'dash.topSenders')}</h2>
          {stats.topSenders.length === 0 ? (
            <EmptyState icon={Users} title={t(locale, 'dash.topSenders')} />
          ) : (
            <ul className="mt-3 space-y-2.5">
              {stats.topSenders.map((sender) => (
                <li key={sender.address} className="flex items-center gap-2.5">
                  <span className="flex size-6 items-center justify-center rounded-md bg-muted text-[10px] font-medium text-muted-foreground">
                    {getInitials(sender.name ?? sender.address)}
                  </span>
                  <span className="flex-1 truncate text-[13px]">
                    {sender.name ?? sender.address}
                  </span>
                  <span className="text-[11.5px] text-muted-foreground">{sender.count}</span>
                </li>
              ))}
            </ul>
          )}
        </section>

        <section className="rounded-[10px] border border-border bg-card px-5 py-4">
          <h2 className="text-[13.5px] font-semibold">{t(locale, 'dash.byAccount')}</h2>
          {accountStats.length === 0 ? (
            <EmptyState icon={Inbox} title={t(locale, 'dash.byAccount')} />
          ) : (
            <div className="mt-3 space-y-3.5">
              {accountStats.map(({ account, unread, total }) => (
                <div key={account.id}>
                  <div className="flex items-baseline justify-between gap-2">
                    <span className="truncate text-[13px]">{account.displayName}</span>
                    <span className="text-[11.5px] text-muted-foreground">
                      {unread} / {total}
                    </span>
                  </div>
                  <div className="mt-1 h-2 rounded bg-accent">
                    <div
                      className="h-2 rounded bg-foreground"
                      style={{ width: `${(total / maxAccountTotal) * 100}%` }}
                    />
                  </div>
                </div>
              ))}
            </div>
          )}
        </section>
      </div>
    </>
  );
}

/**
 * Gap-fill the backend's daily series: zero-mail days are omitted server-side,
 * so build the full window (today-(days-1) … today) and merge counts onto it.
 * Dates use the UTC day basis (`toISOString().slice(0, 10)`) to match the
 * backend's UTC date buckets, so the last entry is "today" as the backend
 * counts it — honestly 0 when no mail arrived today.
 */
function fillDailySeries(daily: DailyVolume[], days: number): DailyVolume[] {
  const countsByDate = new Map(daily.map((d) => [d.date, d.received]));
  const series: DailyVolume[] = [];
  for (let i = days - 1; i >= 0; i--) {
    const date = new Date(Date.now() - i * 86_400_000).toISOString().slice(0, 10);
    series.push({ date, received: countsByDate.get(date) ?? 0 });
  }
  return series;
}

function KpiCard({ label, value, sub }: { label: string; value: React.ReactNode; sub: string }) {
  return (
    <div className="rounded-[10px] border border-border bg-card p-4">
      <p className="text-[10.5px] font-semibold uppercase tracking-[0.8px] text-muted-foreground">
        {label}
      </p>
      <p className="mt-1.5 font-display text-2xl font-medium">{value}</p>
      <p className="mt-0.5 text-[11.5px] text-muted-foreground">{sub}</p>
    </div>
  );
}
