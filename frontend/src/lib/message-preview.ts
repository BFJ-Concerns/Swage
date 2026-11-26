import DOMPurify from "dompurify";
import { Marked, type RendererObject, type Tokens } from "marked";

/** Plain text for the session lists and selector, without link destinations. */
const previewRenderer: RendererObject = {
  heading({ tokens }: Tokens.Heading): string {
    return `${this.parser.parseInline(tokens)} `;
  },

  paragraph({ tokens }: Tokens.Paragraph): string {
    return `${this.parser.parseInline(tokens)} `;
  },

  blockquote({ tokens }: Tokens.Blockquote): string {
    return this.parser.parse(tokens);
  },

  list({ items }: Tokens.List): string {
    return items.map((item) => this.parser.parse(item.tokens)).join(" ") + " ";
  },

  table({ header, rows }: Tokens.Table): string {
    return [header, ...rows]
      .map((row) => row.map((cell) => this.parser.parseInline(cell.tokens)).join(" "))
      .join(" ") + " ";
  },

  strong({ tokens }: Tokens.Strong): string {
    return this.parser.parseInline(tokens);
  },

  em({ tokens }: Tokens.Em): string {
    return this.parser.parseInline(tokens);
  },

  del({ tokens }: Tokens.Del): string {
    return this.parser.parseInline(tokens);
  },

  link({ tokens }: Tokens.Link): string {
    return this.parser.parseInline(tokens);
  },

  image({ tokens }: Tokens.Image): string {
    return this.parser.parseInline(tokens);
  },

  code({ text }: Tokens.Code): string { return `${text} `; },
  codespan({ text }: Tokens.Codespan): string { return text; },
  text(token: Tokens.Text | Tokens.Escape): string {
    return "tokens" in token && token.tokens ? this.parser.parseInline(token.tokens) : token.text;
  },
  html({ text }: Tokens.HTML | Tokens.Tag): string {
    const fragment = DOMPurify.sanitize(text, {
      RETURN_DOM_FRAGMENT: true,
      FORBID_TAGS: ["style"],
    });
    // Text content has no layout: retain the word breaks represented by
    // block elements and explicit line breaks before flattening the fragment.
    for (const element of fragment.querySelectorAll(
      "address, article, aside, blockquote, br, dd, div, dl, dt, figcaption, figure, footer, h1, h2, h3, h4, h5, h6, header, hr, li, main, nav, ol, p, pre, section, table, td, th, tr, ul",
    )) {
      element.prepend(" ");
      element.append(" ");
    }
    return fragment.textContent ?? "";
  },
  br(): string { return " "; },
  hr(): string { return " "; },
  checkbox(): string { return ""; },
};

// A separate parser keeps transcript highlighting and tokenizer overrides out
// of session labels, regardless of which component mounted first.
const previewMarkdown = new Marked({ renderer: previewRenderer, async: false });

/** Flatten a message's Markdown for the named session-preview consumers. */
export function messagePreview(content: string): string {
  return (previewMarkdown.parse(content) as string).replace(/\s+/g, " ").trim();
}
