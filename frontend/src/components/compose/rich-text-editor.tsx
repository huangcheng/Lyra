/**
 * Compose rich-text editor — Plate.js (v53) with a shadcn-styled toolbar.
 *
 * Owns HTML in (deserialize initial value) / HTML out (serializeHtml on
 * change). Toolbar covers the v1 mail set: marks, lists, blockquote, link.
 */

import {
  Bold,
  Italic,
  Link2,
  List,
  ListOrdered,
  Quote,
  Strikethrough,
  Underline,
} from 'lucide-react';
import { useEffect, useRef } from 'react';

import {
  BlockquotePlugin,
  BoldPlugin,
  ItalicPlugin,
  StrikethroughPlugin,
  UnderlinePlugin,
} from '@platejs/basic-nodes/react';
import { insertLink, unwrapLink } from '@platejs/link';
import { LinkPlugin } from '@platejs/link/react';
import { toggleList } from '@platejs/list';
import { ListPlugin } from '@platejs/list/react';
import { HistoryPlugin, HtmlPlugin } from 'platejs';
import {
  ParagraphPlugin,
  Plate,
  PlateContent,
  useEditorRef,
  useEditorSelector,
  usePlateEditor,
} from 'platejs/react';
import { serializeHtml } from 'platejs/static';

import { Button } from '@/components/ui/button';
import { cn } from '@/lib/utils';

export interface RichTextEditorProps {
  /** Initial HTML; only read once when the editor mounts. */
  initialHtml: string;
  onChange: (html: string) => void;
  onKeyDown?: (event: React.KeyboardEvent) => void;
  placeholder?: string;
  className?: string;
  disabled?: boolean;
}

const PLUGINS = [
  ParagraphPlugin,
  HistoryPlugin,
  HtmlPlugin,
  BoldPlugin,
  ItalicPlugin,
  UnderlinePlugin,
  StrikethroughPlugin,
  BlockquotePlugin,
  ListPlugin,
  LinkPlugin,
];

const UL_STYLE = 'disc';
const OL_STYLE = 'decimal';

export function RichTextEditor({
  initialHtml,
  onChange,
  onKeyDown,
  placeholder,
  className,
  disabled,
}: RichTextEditorProps) {
  const editor = usePlateEditor({ plugins: PLUGINS });
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;

  // Seed the initial HTML once (deserialize happens on a live editor).
  useEffect(() => {
    const trimmed = initialHtml.trim();
    if (!trimmed) return;
    const nodes = editor.api.html.deserialize({ element: trimmed });
    if (Array.isArray(nodes) && nodes.length > 0) {
      editor.children = nodes as typeof editor.children;
      (editor.onChange as unknown as () => void)();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [editor]);

  return (
    <div className={cn('rounded-md border border-input', className)}>
      <Plate
        editor={editor}
        readOnly={disabled}
        onChange={() => {
          void serializeHtml(editor).then((html) => onChangeRef.current(html));
        }}
      >
        <Toolbar disabled={disabled} />
        <PlateContent
          className="lyra-editor max-h-72 min-h-32 overflow-y-auto px-3 py-2 text-sm outline-none"
          placeholder={placeholder}
          onKeyDown={onKeyDown}
        />
      </Plate>
    </div>
  );
}

/** Active-block snapshot for toolbar state. */
function useBlockState() {
  return useEditorSelector((ed) => {
    const entry = ed.api.block();
    const node = (entry?.[0] ?? {}) as { type?: string; listStyleType?: string };
    return {
      type: node.type ?? 'p',
      listStyleType: node.listStyleType ?? '',
    };
  }, []);
}

function Toolbar({ disabled }: { disabled?: boolean }) {
  const editor = useEditorRef();
  const block = useBlockState();
  const marks = useEditorSelector(
    (ed) => ({
      bold: Boolean(ed.api.mark(BoldPlugin.key)),
      italic: Boolean(ed.api.mark(ItalicPlugin.key)),
      underline: Boolean(ed.api.mark(UnderlinePlugin.key)),
      strikethrough: Boolean(ed.api.mark(StrikethroughPlugin.key)),
    }),
    [],
  );

  const btn =
    'flex size-7 items-center justify-center rounded-[7px] text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-50';

  const focus = () => editor.tf.focus();

  const toggleMark = (key: string) => {
    editor.tf.toggleMark(key);
    focus();
  };

  const toggleBlockquote = () => {
    if (block.type === BlockquotePlugin.key) {
      editor.tf.setNodes({ type: ParagraphPlugin.key });
    } else {
      editor.tf.setNodes({ type: BlockquotePlugin.key });
    }
    focus();
  };

  const toggleLink = () => {
    const linkEntry = editor.api.node({ match: { type: LinkPlugin.key } });
    if (linkEntry) {
      unwrapLink(editor);
      focus();
      return;
    }
    const url = window.prompt('https://');
    if (!url) return;
    insertLink(editor, { url });
    focus();
  };

  return (
    <div className="flex flex-wrap items-center gap-0.5 border-b border-border/60 px-1.5 py-1">
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className={cn(btn, marks.bold && 'bg-accent text-foreground')}
        disabled={disabled}
        onMouseDown={(e) => e.preventDefault()}
        onClick={() => toggleMark(BoldPlugin.key)}
        aria-label="Bold"
      >
        <Bold className="size-3.5" />
      </Button>
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className={cn(btn, marks.italic && 'bg-accent text-foreground')}
        disabled={disabled}
        onMouseDown={(e) => e.preventDefault()}
        onClick={() => toggleMark(ItalicPlugin.key)}
        aria-label="Italic"
      >
        <Italic className="size-3.5" />
      </Button>
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className={cn(btn, marks.underline && 'bg-accent text-foreground')}
        disabled={disabled}
        onMouseDown={(e) => e.preventDefault()}
        onClick={() => toggleMark(UnderlinePlugin.key)}
        aria-label="Underline"
      >
        <Underline className="size-3.5" />
      </Button>
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className={cn(btn, marks.strikethrough && 'bg-accent text-foreground')}
        disabled={disabled}
        onMouseDown={(e) => e.preventDefault()}
        onClick={() => toggleMark(StrikethroughPlugin.key)}
        aria-label="Strikethrough"
      >
        <Strikethrough className="size-3.5" />
      </Button>
      <span className="mx-1 h-4 w-px bg-border" aria-hidden />
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className={cn(btn, block.listStyleType === UL_STYLE && 'bg-accent text-foreground')}
        disabled={disabled}
        onMouseDown={(e) => e.preventDefault()}
        onClick={() => {
          toggleList(editor, { listStyleType: UL_STYLE });
          focus();
        }}
        aria-label="Bullet list"
      >
        <List className="size-3.5" />
      </Button>
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className={cn(btn, block.listStyleType === OL_STYLE && 'bg-accent text-foreground')}
        disabled={disabled}
        onMouseDown={(e) => e.preventDefault()}
        onClick={() => {
          toggleList(editor, { listStyleType: OL_STYLE });
          focus();
        }}
        aria-label="Numbered list"
      >
        <ListOrdered className="size-3.5" />
      </Button>
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className={cn(btn, block.type === 'blockquote' && 'bg-accent text-foreground')}
        disabled={disabled}
        onMouseDown={(e) => e.preventDefault()}
        onClick={toggleBlockquote}
        aria-label="Blockquote"
      >
        <Quote className="size-3.5" />
      </Button>
      <span className="mx-1 h-4 w-px bg-border" aria-hidden />
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className={btn}
        disabled={disabled}
        onMouseDown={(e) => e.preventDefault()}
        onClick={toggleLink}
        aria-label="Link"
      >
        <Link2 className="size-3.5" />
      </Button>
    </div>
  );
}
