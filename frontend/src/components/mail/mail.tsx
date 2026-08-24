/**
 * Three-pane mail shell from the shadcn v3 mail example.
 * Unified inbox is the default account; per-account folders still work.
 */

import { useEffect, useMemo, useState } from 'react';
import { BarChart3, PenSquare, Search, Settings } from 'lucide-react';
import { useNavigate } from '@tanstack/react-router';
import { useDefaultLayout } from 'react-resizable-panels';

import { AccountSwitcher } from '@/components/mail/account-switcher';
import { MailDisplay } from '@/components/mail/mail-display';
import { MailList } from '@/components/mail/mail-list';
import { SidebarFolders } from '@/components/mail/sidebar-folders';
import { LyraWordmark } from '@/components/lyra-wordmark';
import { ThemeToggle } from '@/components/theme-toggle';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { ResizableHandle, ResizablePanel, ResizablePanelGroup } from '@/components/ui/resizable';
import { Separator } from '@/components/ui/separator';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { TooltipProvider } from '@/components/ui/tooltip';
import { t } from '@/i18n';
import { ALL_ACCOUNTS } from '@/lib/mail-api';
import { useMailData } from '@/lib/use-mail-data';
import { cn } from '@/lib/utils';
import { syncEvents$ } from '@/rxjs/sync-events';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';

/** Green sync dot; amber pulse while any account is syncing. */
function SyncStatusDot() {
  const locale = useUIStore((s) => s.locale);
  const [syncing, setSyncing] = useState(false);

  useEffect(() => {
    const sub = syncEvents$.subscribe((ev) => {
      if (ev.type === 'sync_started') setSyncing(true);
      if (ev.type === 'sync_complete' || ev.type === 'sync_error') setSyncing(false);
    });
    return () => sub.unsubscribe();
  }, []);

  return (
    <span
      className={cn('size-1.5 rounded-full', syncing ? 'animate-pulse bg-unread' : 'bg-ok')}
      role="status"
      aria-label={t(locale, syncing ? 'sync.syncing' : 'sync.syncComplete')}
    />
  );
}

export function Mail() {
  const locale = useUIStore((s) => s.locale);
  const selectedAccountId = useUIStore((s) => s.selectedAccountId);
  const selectedFolderId = useUIStore((s) => s.selectedFolderId);
  const selectedFolderRole = useUIStore((s) => s.selectedFolderRole);
  const setSelectedFolder = useUIStore((s) => s.setSelectedFolder);
  const searchQuery = useUIStore((s) => s.searchQuery);
  const setSearchQuery = useUIStore((s) => s.setSearchQuery);
  const listTab = useUIStore((s) => s.listTab);
  const setListTab = useUIStore((s) => s.setListTab);
  const openCompose = useUIStore((s) => s.openCompose);
  const navigate = useNavigate();
  const folders = useMailStore((s) => s.folders);
  const getFoldersForAccount = useMailStore((s) => s.getFoldersForAccount);
  const accountFolders = useMemo(
    () => (selectedAccountId === ALL_ACCOUNTS ? [] : getFoldersForAccount(selectedAccountId)),
    [folders, getFoldersForAccount, selectedAccountId],
  );

  useMailData();

  useEffect(() => {
    if (selectedAccountId === ALL_ACCOUNTS) return;
    if (selectedFolderId) return;
    const inbox = accountFolders.find((f) => f.role === 'inbox') ?? accountFolders[0];
    if (inbox) setSelectedFolder(inbox.id);
  }, [selectedAccountId, selectedFolderId, accountFolders, setSelectedFolder]);

  const [isCollapsed, setIsCollapsed] = useState(false);

  const { defaultLayout, onLayoutChanged } = useDefaultLayout({
    id: 'lyra-mail',
    storage: typeof window === 'undefined' ? undefined : localStorage,
    panelIds: ['nav', 'list', 'view'],
  });

  const folderTitle = (() => {
    if (selectedFolderRole) {
      return t(locale, `nav.${selectedFolderRole}`);
    }
    if (selectedFolderId) {
      const folder = useMailStore.getState().folders[selectedFolderId];
      if (folder?.role) return t(locale, `nav.${folder.role}`);
      return folder?.name ?? t(locale, 'nav.inbox');
    }
    return t(locale, 'nav.inbox');
  })();

  return (
    <TooltipProvider delayDuration={0}>
      <ResizablePanelGroup
        orientation="horizontal"
        defaultLayout={defaultLayout ?? { nav: 20, list: 32, view: 48 }}
        onLayoutChanged={onLayoutChanged}
        className="h-full items-stretch"
      >
        <ResizablePanel
          id="nav"
          defaultSize="20%"
          collapsedSize={48}
          collapsible
          minSize="15%"
          maxSize="20%"
          onResize={(size) => {
            setIsCollapsed(size.asPercentage < 10 || size.inPixels <= 56);
          }}
          className={cn(isCollapsed && 'min-w-[50px] transition-all duration-300 ease-in-out')}
        >
          <div className="flex h-full flex-col">
            <div
              className={cn(
                'flex h-[52px] items-center justify-center gap-1',
                isCollapsed ? 'h-[52px]' : 'px-2',
              )}
            >
              <div className={cn('min-w-0', !isCollapsed && 'flex-1')}>
                <AccountSwitcher isCollapsed={isCollapsed} />
              </div>
              {isCollapsed ? null : (
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-8 w-8 shrink-0"
                  onClick={() => openCompose()}
                >
                  <PenSquare className="h-4 w-4" />
                  <span className="sr-only">{t(locale, 'mail.compose')}</span>
                </Button>
              )}
            </div>
            <Separator />
            <div className="min-h-0 flex-1 overflow-y-auto">
              <SidebarFolders isCollapsed={isCollapsed} />
            </div>
            {isCollapsed ? (
              <div className="mt-auto flex items-center justify-center px-3 py-2">
                <ThemeToggle isCollapsed />
              </div>
            ) : (
              <div className="mt-auto flex items-center gap-1.5 px-3 py-2">
                <LyraWordmark className="[&>span:last-child]:text-sm" />
                <SyncStatusDot />
                <div className="flex-1" />
                <button
                  type="button"
                  className="inline-flex size-[26px] items-center justify-center rounded-[7px] text-ter-foreground hover:bg-accent"
                  onClick={() => void navigate({ href: '/dashboard' })}
                  aria-label={t(locale, 'nav.dashboard')}
                >
                  <BarChart3 size={14} />
                </button>
                <button
                  type="button"
                  className="inline-flex size-[26px] items-center justify-center rounded-[7px] text-ter-foreground hover:bg-accent"
                  onClick={() => void navigate({ to: '/settings' })}
                  aria-label={t(locale, 'nav.settings')}
                >
                  <Settings size={14} />
                </button>
                <ThemeToggle />
              </div>
            )}
          </div>
        </ResizablePanel>
        <ResizableHandle withHandle />
        <ResizablePanel id="list" defaultSize="32%" minSize="30%">
          <Tabs
            defaultValue="all"
            value={listTab}
            onValueChange={(value) => setListTab(value as 'all' | 'unread')}
            className="flex h-full flex-col gap-0"
          >
            <div className="flex items-center px-4 py-2">
              <h1 className="text-xl font-bold">{folderTitle}</h1>
              <TabsList className="ml-auto h-9 rounded-lg bg-muted p-1 text-muted-foreground">
                <TabsTrigger
                  value="all"
                  className="h-7 flex-none rounded-md px-3 py-1 text-sm font-medium shadow-none data-[state=active]:bg-background data-[state=active]:text-foreground data-[state=active]:shadow-sm"
                >
                  {t(locale, 'mail.allMail')}
                </TabsTrigger>
                <TabsTrigger
                  value="unread"
                  className="h-7 flex-none rounded-md px-3 py-1 text-sm font-medium shadow-none data-[state=active]:bg-background data-[state=active]:text-foreground data-[state=active]:shadow-sm"
                >
                  {t(locale, 'mail.unread')}
                </TabsTrigger>
              </TabsList>
            </div>
            <Separator />
            <div className="bg-background/95 p-4 backdrop-blur supports-backdrop-filter:bg-background/60">
              <form onSubmit={(e) => e.preventDefault()}>
                <div className="relative">
                  <Search className="absolute top-2.5 left-2 h-4 w-4 text-muted-foreground" />
                  <Input
                    placeholder={t(locale, 'mail.searchPlaceholder')}
                    className="pl-8 shadow-sm"
                    value={searchQuery}
                    onChange={(e) => setSearchQuery(e.target.value)}
                  />
                </div>
              </form>
            </div>
            <TabsContent value="all" className="m-0 min-h-0 flex-1">
              <MailList />
            </TabsContent>
            <TabsContent value="unread" className="m-0 min-h-0 flex-1">
              <MailList />
            </TabsContent>
          </Tabs>
        </ResizablePanel>
        <ResizableHandle withHandle />
        <ResizablePanel id="view" defaultSize="48%" minSize="30%">
          <MailDisplay />
        </ResizablePanel>
      </ResizablePanelGroup>
    </TooltipProvider>
  );
}
