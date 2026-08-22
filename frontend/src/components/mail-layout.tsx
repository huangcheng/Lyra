import { Mail } from '@/components/mail/mail';
import { ComposeDialog } from '@/components/compose-dialog';

export function MailLayout() {
  return (
    <div className="flex h-svh flex-col bg-background p-4 md:p-6">
      <div className="min-h-0 flex-1 overflow-hidden rounded-[0.5rem] border bg-background shadow">
        <Mail />
      </div>
      <ComposeDialog />
    </div>
  );
}
