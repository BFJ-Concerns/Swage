# Write Mode

Creating documentation that does not exist. The doctrine's gates all apply; the
decisions specific to write mode are choosing the doc set and shaping the front
door.

## Choosing the Doc Set

The kit is a menu gated by content, not a template to fill. Advisory bands, from
the floor up:

- **Script or single-purpose tool** — README only.
- **Small tool or library** — README, plus at most one reference page if there is
  a real enumerable surface (a CLI, a config file).
- **Service, application, or library with a real surface** — README, then
  guides, reference pages, and a runbook as each is earned: a coherent body of
  grounded content that exists today and fits nowhere else.

Whenever the chosen set includes a `docs/` tree — at any band, a single
earned reference page included — the parallel `agent-docs/` set comes with it
(`parallel-sets.md`), and drafting ends by generating its indexes:
`scripts/doc-indexes.sh` (the verify step's `--check` then proves them
current).

The bands are advisory. An explicit user request for a specific doc overrides
them when real content exists to ground it; a band never licenses an empty file.

**Detect and conform**: where the repo already has documentation conventions — a
docs/ layout, a doc-site tool, heading styles — those win over this file's
defaults. This skill extends an incumbent system; it does not migrate one.

**Source boundary**: existing docs and any Markdown the user supplies are context
for tone, terminology, and conventions — not prose to lift into new documents
unless the user asks for reuse. (Refresh mode preserving already-correct text is
the opposite case, and right.)

## README Shape

The human front door. A shape, not a template — omit any part the project
genuinely lacks:

- What it is and why it exists, in the first paragraph — plain language before
  jargon.
- Install or build, and a quickstart with copy-paste commands that actually work
  (gate 5: extracted, sanity-checked where safe).
- One usage example showing the main workflow.
- A configuration pointer — the couple of options everyone needs, linking to the
  full reference for the rest.
- Links into deeper docs (each reachable from the README directly or via one
  hop) and to the project's status or support expectations where they exist.

Completeness check: purpose, install/build/test/run, configuration, main
workflows, pointers — each either present or genuinely inapplicable.

## Routing by Audience

Every fact routes by who acts on it (doctrine gate 3), and write mode is where
the two audiences become concrete files:

- **Users** — README to orient, docs/ guides and reference to go deep.
  Building from source and operating a deployed instance are user activities
  too: build instructions land in the README's install/build lines and, when
  earned, a how-to guide; test commands ride with them as build verification
  (running the shipped suite proves a from-source build; evolving the suite
  is development process). Capture alongside the build lines the non-obvious
  environment prerequisites — toolchain, platform, system libraries, the
  quirks that defy assumption. Operational procedure lands in the runbook
  (`document-shapes.md`, *Runbooks*). Operational notes are the usual site of
  machine-conditioned facts; doctrine gate 6 governs when they may be written
  and how they are scoped.
- **Coding agents** — two homes, split by when the fact is needed. What an
  agent needs *ambiently in every session* — the build/test/run commands,
  repository layout, standing constraints — is guidance-file material: it
  feeds the agent-guidance hand-off (the skill's Agent-Facing Guidance
  section) and is never written here. What an agent looks up *on demand* —
  reference detail, APIs, configuration surfaces, behaviour — is authored in
  the `agent-docs/` set, the primary rendering the human pages derive from
  (`parallel-sets.md`). Neither gets a narrative home.

Development-process material the survey turns up — release mechanics,
contribution setup, internal design — routes to no document (the skill's
Scope): surface it in the report for the project's planning layer, or into the
agent-guidance hand-off where it is an ambient every-session fact.

## Before Mass Drafting

Propose the doc plan first: the file set with a one-line content statement per
file, plus what is deliberately omitted and why. A plan the user can veto in ten
seconds is cheaper than three drafted files they didn't want.
