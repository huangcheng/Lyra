import { describe, expect, it } from 'vitest';

import { mergeCommands, type CommandDef } from './commands';

const cmd = (id: string, label = id): CommandDef => ({ id, label, onSelect: () => {} });

describe('mergeCommands', () => {
  it('puts page commands first, globals after', () => {
    const merged = mergeCommands([cmd('nav'), cmd('compose')], [cmd('new-event')]);
    expect(merged.map((c) => c.id)).toEqual(['new-event', 'nav', 'compose']);
  });

  it('page command overrides a global with the same id', () => {
    const pageCompose: CommandDef = { ...cmd('compose', 'New event'), hint: 'E' };
    const merged = mergeCommands([cmd('compose', 'New message')], [pageCompose]);
    expect(merged.filter((c) => c.id === 'compose')).toHaveLength(1);
    expect(merged.find((c) => c.id === 'compose')?.label).toBe('New event');
  });
});
