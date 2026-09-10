/**
 * Global command palette (react-cmdk), opened with ⌘K / Ctrl+K.
 *
 * One palette serves every page: global commands (navigate, compose, sync,
 * theme, language, help) are always present, and the active page injects
 * its own group through the `pageCommands` registry (Calendar: new event /
 * today / views; Contacts: new contact). Message full-text search runs
 * from anywhere — picking a hit opens it in Mail.
 */

import { useEffect, useMemo, useRef, useState } from 'react';
import { useNavigate } from '@tanstack/react-router';
import { CommandPalette } from '@/lib/cmdk';
import type { IconName } from 'react-cmdk';
import 'react-cmdk/dist/cmdk.css';
import '@/lib/cmdk-overrides.css';

import { t } from '../i18n';
import { InlineOrb } from '@/components/ui/orb-state';
import { api } from '@/lib/api-client';
import { mapApiMessage, type ApiMessage } from '@/lib/mail-api';
import { mergeCommands, type CommandDef } from '@/lib/commands';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';
import type { ThemeMode } from '@/lib/theme';

interface SearchHit {
  id: string;
  subject: string;
  from: string;
  query: string;
  raw: ApiMessage;
}

export function CommandPaletteRoot() {
  const navigate = useNavigate();
  const locale = useUIStore((s) => s.locale);
  const open = useUIStore((s) => s.paletteOpen);
  const search = useUIStore((s) => s.paletteSearch);
  const setPaletteOpen = useUIStore((s) => s.setPaletteOpen);
  const setPaletteSearch = useUIStore((s) => s.setPaletteSearch);
  const setShortcutHelpOpen = useUIStore((s) => s.setShortcutHelpOpen);
  const pageCommands = useUIStore((s) => s.pageCommands);
  const openCompose = useUIStore((s) => s.openCompose);
  const setTheme = useUIStore((s) => s.setTheme);
  const theme = useUIStore((s) => s.theme);
  const setLocale = useUIStore((s) => s.setLocale);
  const setSelectedMessage = useUIStore((s) => s.setSelectedMessage);
  const accounts = useMailStore((s) => s.accounts);
  const upsertMessage = useMailStore((s) => s.upsertMessage);

  const [hits, setHits] = useState<SearchHit[]>([]);
  const [searching, setSearching] = useState(false);
  const debounceRef = useRef<number | null>(null);

  const q = search.trim();
  useEffect(() => {
    if (debounceRef.current) window.clearTimeout(debounceRef.current);
    if (q.length < 2) {
      setHits([]);
      setSearching(false);
      return;
    }
    setSearching(true);
    debounceRef.current = window.setTimeout(() => {
      void (async () => {
        try {
          const params = new URLSearchParams({ q });
          const data = await api<ApiMessage[]>(`/messages/search?${params}`);
          const seen = new Set<string>();
          setHits(
            data
              .filter((raw) => (seen.has(raw.id) ? false : seen.add(raw.id)))
              .slice(0, 8)
              .map((raw) => {
                let from = raw.fromAddress ?? '';
                try {
                  const parsed = JSON.parse(from) as unknown;
                  const pickAddr = (a: unknown): string => {
                    if (typeof a === 'string') return a;
                    const obj = a as { name?: string; email?: string; raw?: string };
                    return obj.name ?? obj.email ?? obj.raw ?? '';
                  };
                  if (Array.isArray(parsed) && parsed.length > 0) from = pickAddr(parsed[0]);
                  else if (parsed && typeof parsed === 'object') from = pickAddr(parsed);
                } catch {
                  /* bare string */
                }
                return {
                  id: raw.id,
                  subject: raw.subject || '—',
                  from,
                  query: q,
                  raw,
                };
              }),
          );
        } catch {
          setHits([]);
        } finally {
          setSearching(false);
        }
      })();
    }, 250);
    return () => {
      if (debounceRef.current) window.clearTimeout(debounceRef.current);
    };
  }, [q, locale]);

  const openMessage = (hit: SearchHit) => {
    upsertMessage(mapApiMessage(hit.raw));
    setSelectedMessage(hit.id);
    setPaletteOpen(false);
    void navigate({ to: '/' });
  };

  const cycleTheme = () => {
    const order: ThemeMode[] = ['light', 'dark', 'system'];
    setTheme(order[(order.indexOf(theme) + 1) % order.length]!);
  };

  const syncAll = () => {
    void Promise.all(
      accounts.map((a) => api(`/accounts/${a.id}/sync`, { method: 'POST' }).catch(() => null)),
    );
  };

  const globalCommands = useMemo<CommandDef[]>(
    () => [
      {
        id: 'nav-mail',
        label: t(locale, 'palette.nav.mail'),
        icon: 'EnvelopeIcon' as IconName,
        keywords: ['mail', 'inbox', '邮件', '收件箱'],
        onSelect: () => void navigate({ to: '/' }),
      },
      {
        id: 'nav-calendar',
        label: t(locale, 'palette.nav.calendar'),
        icon: 'CalendarDaysIcon' as IconName,
        keywords: ['calendar', 'event', '日历', '日程'],
        onSelect: () => void navigate({ to: '/calendar' }),
      },
      {
        id: 'nav-contacts',
        label: t(locale, 'palette.nav.contacts'),
        icon: 'UsersIcon' as IconName,
        keywords: ['contacts', '通讯录', '联系人'],
        onSelect: () => void navigate({ to: '/contacts' }),
      },
      {
        id: 'nav-dashboard',
        label: t(locale, 'palette.nav.dashboard'),
        icon: 'ChartBarIcon' as IconName,
        keywords: ['dashboard', '仪表盘'],
        onSelect: () => void navigate({ to: '/dashboard' }),
      },
      {
        id: 'nav-settings',
        label: t(locale, 'palette.nav.settings'),
        icon: 'Cog6ToothIcon' as IconName,
        keywords: ['settings', '设置'],
        onSelect: () => void navigate({ to: '/settings' }),
      },
      {
        id: 'compose',
        label: t(locale, 'palette.action.compose'),
        icon: 'PencilSquareIcon' as IconName,
        keywords: ['compose', 'new message', '写邮件', '新建邮件'],
        onSelect: openCompose,
      },
      {
        id: 'sync',
        label: t(locale, 'palette.action.sync'),
        icon: 'ArrowPathIcon' as IconName,
        keywords: ['sync', '同步'],
        onSelect: syncAll,
      },
      {
        id: 'theme',
        label: t(locale, 'palette.action.theme', {
          mode: t(locale, `palette.theme.${theme}`),
        }),
        icon: (theme === 'dark' ? 'SunIcon' : 'MoonIcon') as IconName,
        keywords: ['theme', 'dark', 'light', '主题', '深色'],
        onSelect: cycleTheme,
      },
      {
        id: 'language',
        label: t(locale, 'palette.action.language'),
        icon: 'LanguageIcon' as IconName,
        keywords: ['language', 'english', 'chinese', '语言', '切换'],
        onSelect: () => setLocale(locale === 'zh' ? 'en' : 'zh'),
      },
    ],
    // eslint-disable-next-line react-hooks/exhaustive-deps -- actions close over store fns; locale/theme drive labels
    [locale, theme, accounts],
  );

  const helpCommand = useMemo<CommandDef>(
    () => ({
      id: 'help',
      label: t(locale, 'palette.action.shortcuts'),
      icon: 'QuestionMarkCircleIcon' as IconName,
      keywords: ['shortcuts', 'help', '快捷键', '帮助'],
      onSelect: () => setShortcutHelpOpen(true),
    }),
    [locale, setShortcutHelpOpen],
  );

  const commands = mergeCommands(globalCommands, pageCommands);

  let index = 0;

  return (
    <div className="lyra-cmdk">
      <CommandPalette
        isOpen={open}
        search={search}
        page="root"
        placeholder={t(locale, 'palette.placeholder')}
        onChangeOpen={(o) => setPaletteOpen(o)}
        onChangeSearch={(s) => setPaletteSearch(s)}
      >
        <CommandPalette.Page id="root" onEscape={() => setPaletteOpen(false)}>
          {hits.length > 0 ? (
            <CommandPalette.List heading={t(locale, 'palette.group.search')}>
              {hits.map((hit) => (
                <CommandPalette.ListItem
                  showType={false}
                  key={hit.id}
                  index={index++}
                  icon="MagnifyingGlassIcon"
                  keywords={[hit.query, hit.subject, hit.from]}
                  onClick={() => openMessage(hit)}
                >
                  <span className="flex min-w-0 flex-col">
                    <span className="truncate">{hit.subject}</span>
                    <span className="cmdk-hit-secondary truncate">{hit.from}</span>
                  </span>
                </CommandPalette.ListItem>
              ))}
            </CommandPalette.List>
          ) : null}

          {pageCommands.length > 0 ? (
            <CommandPalette.List heading={t(locale, 'palette.group.page')}>
              {pageCommands.map((cmd) => (
                <CommandPalette.ListItem
                  showType={false}
                  key={cmd.id}
                  index={index++}
                  icon={cmd.icon as IconName}
                  keywords={cmd.keywords}
                  onClick={() => cmd.onSelect()}
                >
                  {cmd.label}
                </CommandPalette.ListItem>
              ))}
            </CommandPalette.List>
          ) : null}

          <CommandPalette.List heading={t(locale, 'palette.group.navigation')}>
            {commands
              .filter((c) => c.id.startsWith('nav-'))
              .map((cmd) => (
                <CommandPalette.ListItem
                  showType={false}
                  key={cmd.id}
                  index={index++}
                  icon={cmd.icon as IconName}
                  keywords={cmd.keywords}
                  onClick={() => cmd.onSelect()}
                >
                  {cmd.label}
                </CommandPalette.ListItem>
              ))}
          </CommandPalette.List>

          <CommandPalette.List heading={t(locale, 'palette.group.actions')}>
            {globalCommands
              .filter((c) => !c.id.startsWith('nav-'))
              .map((cmd) => (
                <CommandPalette.ListItem
                  showType={false}
                  key={cmd.id}
                  index={index++}
                  icon={cmd.icon as IconName}
                  keywords={cmd.keywords}
                  onClick={() => cmd.onSelect()}
                >
                  {cmd.label}
                </CommandPalette.ListItem>
              ))}
            <CommandPalette.ListItem
              showType={false}
              index={index++}
              icon="QuestionMarkCircleIcon"
              keywords={helpCommand.keywords}
              onClick={helpCommand.onSelect}
            >
              {helpCommand.label}
            </CommandPalette.ListItem>
          </CommandPalette.List>

          {searching ? (
            <CommandPalette.List heading="">
              <CommandPalette.ListItem showType={false} index={index++} disabled>
                <InlineOrb state="searching" label={t(locale, 'common.loading')} />
              </CommandPalette.ListItem>
            </CommandPalette.List>
          ) : null}
        </CommandPalette.Page>
      </CommandPalette>
    </div>
  );
}
