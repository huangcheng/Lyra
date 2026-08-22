import { Link } from '@tanstack/react-router';
import type { ReactNode } from 'react';

import { Button } from '@/components/ui/button';
import { t } from '@/i18n';
import { useUIStore } from '@/stores/ui';

export function SecondaryPage({ title, children }: { title: string; children: ReactNode }) {
  const locale = useUIStore((s) => s.locale);

  return (
    <div className="flex h-svh flex-col bg-background">
      <header className="flex h-14 items-center gap-3 border-b px-4">
        <Button variant="ghost" size="sm" asChild>
          <Link to="/">{t(locale, 'common.back')}</Link>
        </Button>
        <h1 className="text-lg font-semibold">{title}</h1>
      </header>
      <div className="flex-1 overflow-auto p-6">{children}</div>
    </div>
  );
}
