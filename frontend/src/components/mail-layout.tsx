import { Mail } from '@/components/mail/mail';
import { ComposeDialog } from '@/components/compose-dialog';

export function MailLayout() {
  return (
    <div className="h-svh bg-background">
      <Mail />
      <ComposeDialog />
    </div>
  );
}
