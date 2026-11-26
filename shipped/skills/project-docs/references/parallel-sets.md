# The two documentation trees

A project with a `docs/` tree keeps two trees, one for each reader, and
indexes generated from them so that no page goes unlisted. A project with
only a README has neither, and the README serves both readers.

## Layout

```
README.md          where a person starts (has no counterpart)
llms.txt           generated index of the agent pages, in the llms.txt format
llms-full.txt      generated: every agent page in one file
docs/              for people: narrative, guides, reference, runbooks
  README.md        index of the human pages: a hand-written opening, then a generated region
agent-docs/        for agents: the same ground, terse
```

The trees mirror each other's paths: `docs/reference/cli.md` pairs with
`agent-docs/reference/cli.md`. A page with no counterpart, such as a tutorial
only a person needs or an operational detail only an agent needs, says so
with `unpaired` in its front matter, so that the index check reads a
declaration and never guesses.

Both trees document using the product. The agent-guidance file sits outside
them: what an agent needs in every session belongs there, and what it looks
up when needed belongs in `agent-docs/`. Which trees a project keeps is the
project's choice, and the index script checks the trees that exist.

## The agent page comes first

Where a fact serves both readers, it is written on the agent page, and the
human page restates it with the narrative, reasons and worked examples the
agent page leaves out. So an update corrects the agent page against the code
first and then re-derives the human pages that restate it. Material only a
person needs (tutorials, why someone would choose one option over another,
conceptual guides) is written directly and declared unpaired.

This is the one case where a fact appears in two files. The agent page
remains its home, and the human page is a derivation that the accuracy
review checks against it. The human page earns its file by being readable:
without it the facts would exist only in the terse form.

An agent page holds what an agent acts on: behaviour, commands,
configuration, interfaces, constraints, and the facts that defy a reasonable
assumption. Each is stated once, in tables where the content is a list, with
no motivation and no walkthrough. Being terse does not excuse an unchecked
claim.

## Front matter

Both trees carry minimal YAML front matter, flat `key: value` lines only:

| Field | Where | Meaning |
|---|---|---|
| `description` | agent pages | One line for `llms.txt`, given only when the page title alone is ambiguous |
| `description` | human pages | One line for the human index, always given |
| `unpaired` | either tree | Why this page has no counterpart |

## The indexes are generated

`scripts/doc-indexes.sh` builds every index from the trees and their front
matter:

- **`llms.txt`**: a hand-written header (an H1 title and a blockquote
  summary) above the marker, then a generated region of link lists under H2
  headings, one entry for each agent page, grouped by subdirectory. An entry
  is the page title linked to its path, followed by its description where the
  front matter gives one.
- **`llms-full.txt`**: the whole agent tree in one file, entirely generated.
- **`docs/README.md`**: a hand-written opening above the marker, then a
  generated region listing every human page with its description.

`scripts/doc-indexes.sh --check` regenerates into a temporary copy and
compares, and it enforces the mirror: every page in either tree has a
counterpart at the mirrored path or an `unpaired` declaration. A stale index,
a page missing from one, or a one-sided page with no declaration fails the
check. The remedy is to regenerate or to declare. A generated region is never
edited by hand, since the next generation overwrites the edit.

## The order of the accuracy review

Each claim is checked against whatever is authoritative for it:

1. **Agent pages against the code.** This is where accuracy is established.
2. **Human pages against their agent counterparts**: the same facts, no
   contradiction, nothing added in the retelling.
3. **Claims only a human page makes against the code**: narrative,
   tutorials and unpaired pages have no agent page to be checked against.

What a claim is checked against is decided claim by claim, and the writer
records it in the inventory (`survey-protocol.md`). A claim that restates its
page's agent counterpart is checked against that counterpart's path. Every
other claim, including narrative added inside a paired page, is checked
against the code. Each step covers the claims this job put in play and is not
an audit of the whole tree. The generated files are renderings of the trees:
check that they regenerate cleanly, and do not review their text.

## A project that has only `docs/`

Do not add the agent tree without saying so. Propose it in the plan, and
build it from the fact inventory: it is written fresh in the terse register
and is not a compression of the human prose. Where the project's own
documentation tooling already produces an
export for agents, use that and do not install this scheme beside it.
