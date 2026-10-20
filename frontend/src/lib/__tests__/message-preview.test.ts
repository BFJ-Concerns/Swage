import { describe, expect, it } from "vitest";
import { messagePreview } from "../message-preview";

describe("message preview plain text", () => {
  it.each([
    ["inline HTML", 'Read <strong title="a > b">these</strong> <em>words</em>.', "Read these words."],
    ["block HTML", '<div><p>First &amp; second</p><p>Next<br>line</p></div><div>Last</div>\n', "First & second Next line Last"],
    ["non-readable HTML", '<div>Visible<script>alert("secret")</script><style>.hidden { color: red }</style><!-- note --></div>\n', "Visible"],
  ])("flattens %s into readable preview text", (_name, source, expected) => {
    expect(messagePreview(source)).toBe(expected);
  });

  it.each([
    ["emphasis", "**Amber Cabinet** - *Size:* __120 × 60 cm__", "Amber Cabinet - Size: 120 × 60 cm"],
    ["code spans", "**Size:** `120 × 60 cm` and ``a ` b``", "Size: 120 × 60 cm and a ` b"],
    ["headings", "## Ingredients\n\nLentils\n=======\n\n### Method", "Ingredients Lentils Method"],
    ["lists", "- **Lentils**\n- Salt\n\n1. Rinse\n2. Cook\n\n- [x] Done", "Lentils Salt Rinse Cook Done"],
    ["tables", "| Name | ID |\n| --- | --- |\n| **Amber** | `42` |", "Name ID Amber 42"],
    ["fences", "```ts\nconst total = 2 * 3;\n```\n\nNext step", "const total = 2 * 3; Next step"],
    ["links", "[Open **child**](swage://session/child-id)", "Open child"],
    ["reference links", "[Open child][child]\n\n[child]: swage://session/child-id", "Open child"],
    ["nested formatting and literal punctuation", "> **Use _hello.txt_**, not ~~old.txt~~.\n\n`a_b # | *` and \\*literal\\*", "Use hello.txt, not old.txt. a_b # | * and *literal*"],
  ])("flattens %s without losing message text", (_name, source, expected) => {
    expect(messagePreview(source)).toBe(expected);
  });
});
