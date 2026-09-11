import { describe, expect, it } from 'vitest';

import { matchMailer } from './mailer';

describe('matchMailer', () => {
  it('matches Thunderbird User-Agent', () => {
    const m = matchMailer(
      'Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Thunderbird/128.0',
    );
    expect(m).toEqual({ id: 'thunderbird', name: 'Thunderbird' });
  });

  it('matches Apple Mail with version suffix', () => {
    expect(matchMailer('Apple Mail (2.3774.400.31)')?.id).toBe('apple-mail');
  });

  it('matches Outlook variants including X-MimeOLE lineage', () => {
    expect(matchMailer('Microsoft Outlook 16.0.1234')?.id).toBe('outlook');
    expect(matchMailer('Microsoft Office Outlook 12.0')?.id).toBe('outlook');
    expect(matchMailer('Produced By Microsoft MimeOLE V6.00')?.id).toBe('outlook');
  });

  it('matches CJK desktop clients', () => {
    expect(matchMailer('Foxmail 7.2.25.254[en]')?.id).toBe('foxmail');
    expect(matchMailer('X-Mailer: 网易邮箱大师 7.0')?.id).toBe('netease');
  });

  it('matches terminal clients', () => {
    expect(matchMailer('Mutt/2.2.9 (2022-11-12)')?.id).toBe('mutt');
    expect(matchMailer('Heirloom mailx 12.5 7/5/10')?.id).toBe('mutt');
  });

  it('matches webmail backends worth naming', () => {
    expect(matchMailer('Yamail [ http://yandex.ru ] 5.0')?.name).toBe('Yandex Mail');
    expect(matchMailer('Coremail Webmail Server Version 2024.2')?.name).toBe('Coremail');
    expect(matchMailer('Zendesk Mailer')?.name).toBe('Zendesk');
  });

  it('ignores sending libraries (not MUAs)', () => {
    expect(matchMailer('go-mail v0.7.0 // https://github.com/wneessen/go-mail')).toBeNull();
    expect(matchMailer('CodeIgniter')).toBeNull();
  });

  it('returns null for absent or empty values', () => {
    expect(matchMailer(null)).toBeNull();
    expect(matchMailer(undefined)).toBeNull();
    expect(matchMailer('   ')).toBeNull();
  });

  it('returns null for unrecognized mailers (render nothing)', () => {
    expect(matchMailer('Postfix (cowbell)')).toBeNull();
    expect(matchMailer('SomeRandomMTA/1.0')).toBeNull();
  });

  it('is case-insensitive', () => {
    expect(matchMailer('THUNDERBIRD/128.0')?.id).toBe('thunderbird');
  });
});
