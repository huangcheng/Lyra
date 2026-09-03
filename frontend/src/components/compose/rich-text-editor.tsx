/**
 * Compose rich-text editor — Plate.js (v53) with a shadcn-styled toolbar.
 *
 * Owns HTML in (deserialize initial value) / HTML out (serializeHtml on
 * change). Toolbar covers the v1 mail set: marks, lists, blockquote, link.
 * Markdown works both ways users expect: paste `**bold**`/`- list`/`> quote`
 * and it lands formatted (MarkdownPlugin), and typing markdown shortcuts
 * auto-formats (per-plugin `inputRules` — @platejs/autoformat is inert in v53).
 */

import {
  Bold,
  Image as ImageIcon,
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
import {
  BlockquoteRules,
  BoldRules,
  ItalicRules,
  StrikethroughRules,
  UnderlineRules,
} from '@platejs/basic-nodes';
import { insertLink, unwrapLink } from '@platejs/link';
import { LinkPlugin } from '@platejs/link/react';
import { toggleList } from '@platejs/list';
import { ListPlugin } from '@platejs/list/react';
import { BulletedListRules, OrderedListRules } from '@platejs/list';
import { MarkdownPlugin } from '@platejs/markdown';
import { ImagePlugin } from '@platejs/media/react';
import { HistoryPlugin, HtmlPlugin } from 'platejs';
import {
  ParagraphPlugin,
  Plate,
  PlateContent,
  useEditorRef,
  useEditorSelector,
  usePlateEditor,
} from 'platejs/react';
import type { PlateElementProps } from 'platejs/react';
import { serializeHtml } from 'platejs/static';

import { Button } from '@/components/ui/button';
import { t } from '@/i18n';
import { cn } from '@/lib/utils';
import { useUIStore } from '@/stores/ui';

export interface RichTextEditorProps {
  /** Initial HTML; only read once when the editor mounts. */
  initialHtml: string;
  onChange: (html: string) => void;
  onKeyDown?: (event: React.KeyboardEvent) => void;
  placeholder?: string;
  className?: string;
  /** Class override for the content area (e.g. a shorter reply box). */
  contentClassName?: string;
  /** Toolbar placement; compose uses a bottom bar, reply keeps the top. */
  toolbarPosition?: 'top' | 'bottom';
  disabled?: boolean;
  /**
   * Image file from toolbar/paste/drop → object URL to display, or null to
   * reject (size/type). Ownership of the URL stays with the caller.
   */
  onImageFile?: (file: File) => string | null;
}

/** Minimal void image node; children carry Slate's hidden selection text. */
function ImageElement({ attributes, children, element }: PlateElementProps) {
  return (
    <div {...attributes} className="my-1 select-none">
      <img
        src={(element as { url?: string }).url}
        alt=""
        className="max-h-64 max-w-full rounded"
        contentEditable={false}
      />
      {children}
    </div>
  );
}

const PLUGINS = [
  ParagraphPlugin,
  HistoryPlugin,
  HtmlPlugin,
  // Paste Markdown → rich text; typing shortcuts come from each feature
  // plugin's `inputRules` (Plate v53: @platejs/autoformat is inert).
  MarkdownPlugin,
  BoldPlugin.configure({ inputRules: [BoldRules.markdown()] }),
  ItalicPlugin.configure({ inputRules: [ItalicRules.markdown()] }),
  UnderlinePlugin.configure({ inputRules: [UnderlineRules.markdown()] }),
  StrikethroughPlugin.configure({ inputRules: [StrikethroughRules.markdown()] }),
  BlockquotePlugin.configure({ inputRules: [BlockquoteRules.markdown()] }),
  ListPlugin.configure({
    inputRules: [BulletedListRules.markdown(), OrderedListRules.markdown()],
  }),
  LinkPlugin,
  ImagePlugin.configure({
    render: { node: ImageElement },
  }),
];

const UL_STYLE = 'disc';
const OL_STYLE = 'decimal';

export function RichTextEditor({
  initialHtml,
  onChange,
  onKeyDown,
  placeholder,
  className,
  contentClassName,
  toolbarPosition = 'top',
  disabled,
  onImageFile,
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

  const insertImageFile = (file: File) => {
    if (!onImageFile || !file.type.startsWith('image/')) return;
    const url = onImageFile(file);
    if (!url) return; // rejected (too large) — the caller surfaced the error
    editor.tf.insertNodes({ type: ImagePlugin.key, url, children: [{ text: '' }] });
  };

  const imageFilesFrom = (files: FileList | null | undefined) =>
    Array.from(files ?? []).filter((f) => f.type.startsWith('image/'));

  const toolbarProps = { disabled, onImageFile, insertImageFile };

  return (
    <div className={cn('rounded-md border border-input', className)}>
      <Plate
        editor={editor}
        readOnly={disabled}
        onChange={() => {
          void serializeHtml(editor).then((html) => onChangeRef.current(html));
        }}
      >
        {toolbarPosition === 'top' ? <Toolbar {...toolbarProps} position="top" /> : null}
        <PlateContent
          className={cn(
            'lyra-editor max-h-72 min-h-32 overflow-y-auto px-3 py-2 text-sm outline-none',
            contentClassName,
          )}
          placeholder={placeholder}
          onKeyDown={onKeyDown}
          onPaste={(e) => {
            const files = imageFilesFrom(e.clipboardData?.files);
            if (files.length > 0) {
              e.preventDefault();
              files.forEach(insertImageFile);
            }
          }}
          onDrop={(e) => {
            const files = imageFilesFrom(e.dataTransfer?.files);
            if (files.length > 0) {
              e.preventDefault();
              files.forEach(insertImageFile);
            }
          }}
        />
        {toolbarPosition === 'bottom' ? <Toolbar {...toolbarProps} position="bottom" /> : null}
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

function Toolbar({
  disabled,
  position = 'top',
  onImageFile,
  insertImageFile,
}: {
  disabled?: boolean;
  position?: 'top' | 'bottom';
  onImageFile?: (file: File) => string | null;
  insertImageFile?: (file: File) => void;
}) {
  const editor = useEditorRef();
  const block = useBlockState();
  const locale = useUIStore((s) => s.locale);
  const imageInputRef = useRef<HTMLInputElement>(null);
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
    <div
      className={cn(
        'flex flex-wrap items-center gap-0.5 px-1.5 py-1',
        position === 'top' ? 'border-b border-border/60' : 'border-t border-border/60',
      )}
    >
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
      <input
        ref={imageInputRef}
        type="file"
        accept="image/*"
        multiple
        className="hidden"
        onChange={(e) => {
          Array.from(e.target.files ?? []).forEach((f) => insertImageFile?.(f));
          e.target.value = '';
        }}
      />
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className={btn}
        disabled={disabled || !onImageFile}
        onMouseDown={(e) => e.preventDefault()}
        onClick={() => imageInputRef.current?.click()}
        aria-label={t(locale, 'mail.insertImage')}
      >
        <ImageIcon className="size-3.5" />
      </Button>
    </div>
  );
}
