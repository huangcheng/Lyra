/**
 * Three-pane mail shell from the shadcn v3 mail example.
 * Unified inbox is the default account; per-account folders still work.
 */

import { useEffect, useMemo, useState } from 'react';
import {
  Archive,
  ArchiveX,
  Calendar,
  File,
  Inbox,
  PenSquare,
  Search,
  Send,
  Settings,
  Trash2,
  Users,
} from 'lucide-react';
import { useDefaultLayout } from 'react-resizable-panels';

import { AccountSwitcher } from '@/components/mail/account-switcher';
import { FolderNavTree } from '@/components/mail/folder-nav-tree';
import { MailDisplay } from '@/components/mail/mail-display';
import { MailList } from '@/components/mail/mail-list';
import { Nav } from '@/components/mail/nav';
import { LyraWordmark } from '@/components/lyra-wordmark';
import { ThemeToggle } from '@/components/theme-toggle';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { ResizableHandle, ResizablePanel, ResizablePanelGroup } from '@/components/ui/resizable';
import { Separator } from '@/components/ui/separator';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { TooltipProvider } from '@/components/ui/tooltip';
import { t } from '@/i18n';
import { ALL_ACCOUNTS, type StandardFolderRole } from '@/lib/mail-api';
import { buildCustomFolderTree } from '@/lib/folder-tree';
import { useMailData } from '@/lib/use-mail-data';
import { cn } from '@/lib/utils';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';

const ROLE_ICONS: Record<StandardFolderRole, typeof Inbox> = {
  inbox: Inbox,
  drafts: File,
  sent: Send,
  spam: ArchiveX,
  trash: Trash2,
  archive: Archive,
};

export function Mail() {
  const locale = useUIStore((s) => s.locale);
  const selectedAccountId = useUIStore((s) => s.selectedAccountId);
  const selectedFolderId = useUIStore((s) => s.selectedFolderId);
  const selectedFolderRole = useUIStore((s) => s.selectedFolderRole);
  const setSelectedFolder = useUIStore((s) => s.setSelectedFolder);
  const setSelectedFolderRole = useUIStore((s) => s.setSelectedFolderRole);
  const searchQuery = useUIStore((s) => s.searchQuery);
  const setSearchQuery = useUIStore((s) => s.setSearchQuery);
  const listTab = useUIStore((s) => s.listTab);
  const setListTab = useUIStore((s) => s.setListTab);
  const openCompose = useUIStore((s) => s.openCompose);
  const folders = useMailStore((s) => s.folders);
  const getUnifiedFolders = useMailStore((s) => s.getUnifiedFolders);
  const getFoldersForAccount = useMailStore((s) => s.getFoldersForAccount);
  const unifiedFolders = useMemo(() => getUnifiedFolders(), [folders, getUnifiedFolders]);
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

  const primaryLinks =
    selectedAccountId === ALL_ACCOUNTS
      ? unifiedFolders.map((folder) => ({
          title: t(locale, `nav.${folder.role}`),
          label: folder.unreadCount > 0 ? String(folder.unreadCount) : '',
          icon: ROLE_ICONS[folder.role],
          variant: (selectedFolderRole === folder.role ? 'default' : 'ghost') as
            'default' | 'ghost',
          onClick: () => setSelectedFolderRole(folder.role),
        }))
      : accountFolders
          .filter((folder) => folder.role)
          .map((folder) => ({
            title: folder.role ? t(locale, `nav.${folder.role}`) : folder.name,
            label: folder.unreadCount > 0 ? String(folder.unreadCount) : '',
            icon: ROLE_ICONS[(folder.role ?? 'inbox') as StandardFolderRole] ?? File,
            variant: (selectedFolderId === folder.id ? 'default' : 'ghost') as 'default' | 'ghost',
            onClick: () => setSelectedFolder(folder.id),
          }));

  const customFolderTree = useMemo(
    () =>
      selectedAccountId === ALL_ACCOUNTS ? [] : buildCustomFolderTree(accountFolders, folders),
    [selectedAccountId, accountFolders, folders],
  );

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
            <Nav isCollapsed={isCollapsed} links={primaryLinks} />
            {customFolderTree.length > 0 ? (
              <>
                <Separator />
                <FolderNavTree
                  isCollapsed={isCollapsed}
                  nodes={customFolderTree}
                  selectedFolderId={selectedFolderId}
                  onSelect={setSelectedFolder}
                />
              </>
            ) : null}
            <Separator />
            <Nav
              isCollapsed={isCollapsed}
              links={[
                {
                  title: t(locale, 'nav.contacts'),
                  icon: Users,
                  variant: 'ghost',
                  href: '/contacts',
                },
                {
                  title: t(locale, 'nav.calendar'),
                  icon: Calendar,
                  variant: 'ghost',
                  href: '/calendar',
                },
                {
                  title: t(locale, 'nav.settings'),
                  icon: Settings,
                  variant: 'ghost',
                  href: '/settings',
                },
              ]}
            />
            <div className="mt-auto flex items-center justify-between px-3 py-2">
              {isCollapsed ? null : <LyraWordmark className="[&>span:last-child]:text-sm" />}
              <ThemeToggle isCollapsed={isCollapsed} />
            </div>
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
