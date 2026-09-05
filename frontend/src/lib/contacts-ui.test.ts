import { describe, expect, it } from 'vitest';

import {
  addressbookLabelFromUrl,
  contactLetter,
  filterContacts,
  groupContactsByLetter,
  uniqueAddressbooks,
} from './contacts-ui';

const sample = [
  {
    id: '1',
    accountId: 'a1',
    displayName: 'Alice',
    emailAddresses: ['alice@example.com'],
    addressbookUrl: 'https://carddav.fastmail.com/dav/addressbooks/user/x/Default/',
  },
  {
    id: '2',
    accountId: 'a1',
    displayName: 'Bob',
    emailAddresses: ['bob@example.com'],
    addressbookUrl: 'https://carddav.fastmail.com/dav/addressbooks/user/x/Default/',
  },
  {
    id: '3',
    accountId: 'a1',
    displayName: '2Cool',
    emailAddresses: [],
    addressbookUrl: 'https://carddav.fastmail.com/dav/addressbooks/user/x/owner@x.Shared/',
  },
  {
    id: '4',
    accountId: 'a2',
    displayName: 'Carol',
    emailAddresses: ['carol@example.com'],
    addressbookUrl: null,
  },
];

describe('addressbookLabelFromUrl', () => {
  it('maps Fastmail Default → Personal and Shared → Shared', () => {
    expect(
      addressbookLabelFromUrl('https://carddav.fastmail.com/dav/addressbooks/user/u@d/Default'),
    ).toBe('Personal');
    expect(
      addressbookLabelFromUrl('https://carddav.fastmail.com/dav/addressbooks/user/u@d/u@d.Shared/'),
    ).toBe('Shared');
  });

  it('falls back to the last path segment', () => {
    expect(addressbookLabelFromUrl('/dav/ab/Work/')).toBe('Work');
  });
});

describe('groupContactsByLetter', () => {
  it('buckets under A–Z and #', () => {
    const groups = groupContactsByLetter(sample);
    expect(groups.map((g) => g.letter)).toEqual(['#', 'A', 'B', 'C']);
    expect(groups.find((g) => g.letter === 'A')?.contacts.map((c) => c.id)).toEqual(['1']);
  });
});

describe('contactLetter', () => {
  it('uses # for names starting with digits', () => {
    expect(contactLetter(sample[2]!)).toBe('#');
  });
});

describe('filterContacts', () => {
  it('filters by account + addressbook and search', () => {
    const book = filterContacts(
      sample,
      {
        accountId: 'a1',
        addressbookUrl: sample[0]!.addressbookUrl!,
      },
      '',
    );
    expect(book.map((c) => c.id)).toEqual(['1', '2']);
    const q = filterContacts(sample, 'all', 'bob');
    expect(q.map((c) => c.id)).toEqual(['2']);
  });
});

describe('uniqueAddressbooks', () => {
  it('dedupes books with Personal/Shared labels', () => {
    const books = uniqueAddressbooks(sample);
    expect(books).toHaveLength(2);
    expect(books.map((b) => b.label).sort()).toEqual(['Personal', 'Shared']);
  });
});
