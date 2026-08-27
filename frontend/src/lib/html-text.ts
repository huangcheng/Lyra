/**
 * HTML → plain-text conversion for `multipart/alternative` bodies.
 *
 * Block-level elements separate paragraphs; `<br>` breaks lines; list items
 * get a bullet prefix. Enough fidelity for a text alternative, no ambitions
 * of layout fidelity.
 */
export function htmlToText(html: string): string {
  if (!html) return '';
  const doc = new DOMParser().parseFromString(html, 'text/html');

  const renderInline = (node: Node): string => {
    let out = '';
    node.childNodes.forEach((child) => {
      if (child.nodeType === Node.TEXT_NODE) {
        out += child.textContent ?? '';
        return;
      }
      if (child.nodeType !== Node.ELEMENT_NODE) return;
      const el = child as HTMLElement;
      const tag = el.tagName.toLowerCase();
      if (tag === 'br') {
        out += '\n';
        return;
      }
      if (tag === 'a') {
        const href = el.getAttribute('href') ?? '';
        const label = renderInline(el);
        out += href && href !== label ? `${label} <${href}>` : label;
        return;
      }
      out += renderInline(el);
    });
    return out;
  };

  const renderBlock = (el: Element): string[] => {
    const tag = el.tagName.toLowerCase();
    if (tag === 'ul' || tag === 'ol') {
      const lines: string[] = [];
      el.querySelectorAll(':scope > li').forEach((li) => {
        lines.push(`• ${renderInline(li).trim()}`);
      });
      return lines;
    }
    if (tag === 'blockquote') {
      const inner = renderBlocks(el).join('\n');
      return inner
        .split('\n')
        .map((l) => `> ${l}`.trimEnd())
        .concat('');
    }
    const text = renderInline(el)
      .replace(/\u00a0/g, ' ')
      .trim();
    return [text];
  };

  const renderBlocks = (parent: Element): string[] => {
    const lines: string[] = [];
    parent.childNodes.forEach((child) => {
      if (child.nodeType === Node.TEXT_NODE) {
        const t = (child.textContent ?? '').trim();
        if (t) lines.push(t);
        return;
      }
      if (child.nodeType !== Node.ELEMENT_NODE) return;
      lines.push(...renderBlock(child as Element));
    });
    return lines;
  };

  return renderBlocks(doc.body)
    .join('\n')
    .replace(/\n{3,}/g, '\n\n')
    .trim();
}
