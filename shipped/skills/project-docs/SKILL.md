---
name: Project Docs
description: Write or refresh a project's repository Markdown documentation — README.md, a docs/ tree scaled to the project (how-to guides, reference tables, runbooks), and the parallel agent-facing set (agent-docs/) with generated llms.txt indexes — grounded in the implementation as it actually is, coordinating the agent-guidance hand-off. The docs cover using the product — installing, configuring, operating, building from source — not the project's own development process; such material found in existing docs is stripped on refresh. Use when asked to document a project or repo, write or update the README or docs, create or refresh agent-facing docs or an llms.txt index, bring stale documentation up to scratch, or sync docs after a feature lands. Not for code comments or docstrings, CHANGELOG generation, LICENSE or CONTRIBUTING files, doc-site tooling such as mkdocs or docusaurus, or work solely on the agent-guidance file (AGENTS.md / CLAUDE.md) — a dedicated skill owns it.
---

# Project Docs

Produce and maintain a project's documentation as a map of implemented reality:
accurate to the code at HEAD, scaled to the project's real size, and entirely
about **using the product** — installing, configuring, driving, operating,
building from source — readable by its two audiences: human users (narrative,
rationale, guides) and coding agents (terse operational fact), with every
substantive fact holding one canonical home. How the product is developed —
contribution setup, release mechanics, internal design — is not documentation
material: developer knowledge lives in the project's planning layer and the
agent-guidance file, and reaches those homes through the report and the
agent-guidance hand-off rather than through pages in the docs trees.

Two modes share this doctrine. **Write** creates documentation that does not
exist; **refresh** audits existing documentation against the current
implementation and brings it up to scratch. Most real requests are hybrids; see
Mode Selection.

## Scope

Decline mis-triggers early, with a pointer, rather than after survey work:

- **Code comments and docstrings** — ordinary coding work, not this skill.
- **CHANGELOG and release notes** — release tooling's job.
- **Development-process documentation** — contribution setup, development
  workflow, release mechanics, internal design and decision records — is never
  written: repository docs document using the product. Found during a refresh
  in the docs the job's scope covers, such material is stripped — delete it,
  recording each strip in the drift report and surfacing anything the planning
  layer or the agent-guidance hand-off should keep. The strip reaches README,
  docs/, and agent-docs/ pages only: root-level community and legal files
  (CONTRIBUTING.md, LICENSE, forge templates) are outside this skill's
  authority in both directions — never created, never deleted; note them in
  the report instead.
- **LICENSE and CONTRIBUTING** — never create these; legal and contribution terms
  are the user's decision, not a documentation gap.
- **Doc-site tooling** (mkdocs, docusaurus, and kin) — never set up or migrate it.
  When a project already uses one, conform to its conventions.
- **Planning artefacts** — roadmaps, task lists, and issue logs live in the
  project's planning layer, not in repo docs. Planning content found *inside*
  docs is relocated there or surfaced in the report — never silently dropped,
  never left in place.
- **The agent-guidance file's internals** — coordinated through the hand-off in
  Agent-Facing Guidance below, not authored freestyle here.

## Doctrine

Six gates, each stated with the unaided default it corrects:

1. **Evidence.** The default is documenting what the project intends. Rule:
   document only what is implemented and reachable at HEAD; TODOs, roadmap
   comments, dead code, and feature-flagged-off paths are not evidence.
   Substantive claims — behaviour, commands, config, APIs,
   dependencies — trace to a source read this session; connective prose is
   exempt. Test: name the file that makes this sentence true, or cut the
   sentence.
2. **Earn its file.** The default is scaffolding a full docs/ kit. Rule:
   README-only is the floor and the default; a docs/ file exists only when a
   coherent body of grounded content exists today and fits nowhere else. Absence
   beats scaffold: no empty sections, no placeholders. An explicit user request
   for a specific doc overrides the default — provided real content exists to
   ground it. Test: would deleting this file lose information that exists
   today? (A parallel-set derived page passes by its rendering —
   `references/parallel-sets.md`.)
3. **One canonical home, signposts elsewhere.** The default is restating the
   build commands in README, docs/, and the agent-guidance file until the copies
   drift apart. Rule: a fact's authoritative substance — exact values, tables,
   full procedures — lives in one file, chosen by who acts on it: executing
   agents → the agent-guidance file for ambient every-session facts, the
   `agent-docs/` set for on-demand reference (`references/parallel-sets.md`);
   orienting users → README; deep-diving
   users → docs/ guides and reference; users operating a deployed instance →
   the runbook (`docs/runbook.md` and kin).
   Other files carry a one-line signpost or a short excerpt with
   a link. The README keeps copy-paste install/run/test lines even when the
   canonical detail lives elsewhere — a front door that is only links orients
   nobody. The one sanctioned exception is the parallel-set derivation
   (`references/parallel-sets.md`): a human page restating its agent
   counterpart is a governed derivation with a declared authority, not a
   second home. Test: when this fact changes, exactly one authoritative edit.
4. **Patch, don't rewrite** (refresh). The default is regenerating the file.
   Rule: the unit of change is the wrong claim; make minimal semantic edits and
   preserve correct prose, voice, terminology, and heading style. A whole-file
   rewrite is allowed only when targeted patches cannot leave the file clear —
   declare that in the drift report before doing it. Never overwrite a file you
   have not read.
5. **Extract, don't recall.** The default is typing reference entries from
   memory — which invents flags, defaults, and error codes. Rule: every
   reference-table row comes from a reproducible source — the arg parser, config
   struct or schema, error enum, generated metadata, or a safe command actually
   run — and the fact inventory records which. Match the code's exact spelling:
   if the code says `timeout_ms`, the doc says `timeout_ms`. No source, no row.
   Never hand-edit generated documentation sections — update the generator's
   input, or report the blocker.
6. **Scope machine-conditioned facts.** The default is writing operational
   notes — sudo specifics, systemd units, native services, deployment paths —
   as if they were true everywhere the project runs. Rule: a fact conditioned
   on a particular machine or deployment is written with its scope named ("on
   the production host…", "under the bundled systemd unit…"), never as general
   truth; and propose such a section only when this session can verify the
   claims on the machine they describe — otherwise leave it to a session that
   can, noting the gap in the report. Test: would this sentence still be true
   in a fresh clone on another machine — and if not, does it say so?

Write plain GitHub-flavoured Markdown: relative links, standard headings, an H1
title per document. GFM extensions the host forge renders are welcome in
moderation — alert blockquotes (`> [!NOTE]`, `> [!WARNING]`) for genuine tips
and cautions, collapsible `<details>` sections for bulky optional content. No
wikilinks, and no frontmatter beyond the index schema
(`references/parallel-sets.md`, Frontmatter).

## Two Registers

The two audiences take two registers, which fail differently and
conflict in their fixes; fix a document's register by its primary reader
before drafting:

- **Agent-facing** — the agent-guidance file, and any reference whose reader is
  an executing agent. This is instruction text: write it with prompt craft —
  explicit scope, mechanism-grounded reasons, one term per concept, no
  decorative prose.
- **Human-facing** — README narrative, guides, rationale, and the runbooks.
  Draft it to read as written
  rather than generated: read `references/human-register.md` before drafting
  or materially revising human-facing prose, and hold its patterns out from
  the start.

Both registers take a meaning-preserving clarity pass — simpler, more precise
wording that changes no claim — after drafting and before the accuracy review.

Run the pass yourself as a separate edit — content and structure settled
first, the clarity edit second: simpler and more precise wording only,
changing no claim.

Past the README-only floor the registers become parallel documentation trees —
a `docs/` human set and a sibling `agent-docs/` agent set, the agent set
primary and the human set derived from it, with deterministically generated
indexes (`llms.txt`, `llms-full.txt`, the human contents page). Read
`references/parallel-sets.md` whenever the project keeps or earns a `docs/`
tree; content is never written in a compromise register.

## Mode Selection

- **Refresh** when the request concerns the accuracy of documentation that
  exists; the deliverable is a drift report plus targeted patches.
- **Write** for documentation that does not exist; the deliverable is the docs.
- **Hybrid** (the common case): audit whatever exists refresh-style first; author
  only files that do not exist.
- Scoped asks ("the README is stale") audit the named file plus the facts it
  shares homes with. Whole-repo asks audit README, docs/, and the agent-guidance
  file at repo scale.
- A request for a **single document** establishes its type, audience, goal, and
  scope in and out before drafting — ask when the request leaves them unclear —
  and proposes the document's outline before writing it in full.

## Workflow

Five steps whose depth scales with the ask — a scoped README refresh collapses to
a short audit-and-patch; a whole-repo write runs the full survey:

1. **Inventory** existing docs and discover the project's surfaces: Markdown
   files, manifests, CI configs, entry points, CLI/config/API definitions,
   existing agent-guidance files.
2. **Survey** the code for evidence, building a working fact inventory — claim,
   source (file or command), audience, canonical home. The inventory is expected
   for multi-file doc sets; informal notes suffice for a README-only job. Keep it
   in the session's scratch space, never committed to the repo. Read
   `references/survey-protocol.md` before any survey of substance.
3. **Plan.** Route facts to homes and fix the doc set (gate 2). For anything
   beyond a single-file patch, propose the plan — files plus a one-line content
   statement each — before mass drafting.
4. **Draft or patch** — a single writer transcribing from the inventory, in the
   register the document's reader fixes (Two Registers); refresh mode writes
   the drift report before editing. Read `references/workflow-write.md` or
   `references/workflow-refresh.md` for the mode's full procedure on
   non-trivial jobs, and `references/document-shapes.md` at drafting time for
   the shape of each earned file.
5. **Verify.** Run the accuracy review (below), check relative links with
   `scripts/check-doc-links.sh`, regenerate and check the indexes with
   `scripts/doc-indexes.sh --check` where the project keeps parallel sets,
   spot-audit duplication (exact values and
   procedures single-homed; signposts fine), and sanity-check documented commands
   where safe — read-only invocations such as `--help`; anything heavier only
   when it is already part of the session's work. Then commit the documentation
   changes as part of completion, unless the user or the project's policy holds
   commits.

## Delegation

Parallelism is for reading, never writing: the breadth survey and per-claim
verification fan out; synthesis, authorship, and review verdicts stay with one
strong agent, so the docs speak with one voice and components are described once,
consistently. Workers return structured evidence — claim, citation, uncertainty —
never prose to paste; their stable instructions live in `prompts/`.

Prefer a workflow for that delegation (the workflow-authoring skill documents
the surface): cheap-tier workers for the survey fan-out, and the accuracy
review on the opposite model family from the writer, so a differently-trained
family checks the claims rather than the writer's own. When the catalogue
offers only one family, fall back to delegated clean-context workers for the
fan-out and a fresh delegated worker for the accuracy review, prompted from
`prompts/accuracy-review.md` with the claim list — a delegated worker shares no
session context, so the review is still de-anchored from the writer's
reasoning; note in the report that the review was same-family.

With no delegation mechanism at all, run the same workflow sequentially and note
in the report that the accuracy review was a same-context self-review.

## Agent-Facing Guidance

The agent-facing guidance file (AGENTS.md and its pointers) is part of the doc
kit but not authored freestyle here: the `agent-guidance` skill owns its
structure and conventions. During the survey, compile the sourced agent-facing
facts — commands, layout, constraints, conventions — with their canonical homes,
then invoke `agent-guidance` to apply them; it decides the file's internals.
Keep the register split intact: the guidance file carries terse operational fact
and points into README and docs/ for narrative — it never duplicates them. If
the skill is unavailable in the session, list the compiled facts in the final
report as a pending hand-off rather than editing the file directly — a second
freestyle writer to that file recreates exactly the drift this skill exists to
remove.

## Verification

The accuracy review is the gate between drafting and done: an adversarial
reviewer receives the claim list and repo access — not the writer's reasoning —
and tries to falsify each claim (`prompts/accuracy-review.md`). Under parallel
sets the review runs as a chain — agent set against code, derived human pages
against their agent counterparts, human-only claims against code
(`references/parallel-sets.md`, The verification chain). Verdicts are
terminal: falsified and unverifiable claims are corrected or removed before the
docs ship — never hedged, never shipped flagged.

## Report

The final report states: files created and changed; the drift report (refresh);
claims removed or omitted for lack of evidence; development-process content
stripped, with anything surfaced for the planning layer or the agent-guidance
hand-off; docs deliberately not created and
why; agent-facing facts handed off or listed as pending; and the verification
performed with its limits (commands not run, areas not readable). The drift
report is conversation or PR-description material — do not commit it to the repo.

## References

- `references/survey-protocol.md` — evidence classes and precedence, the fact
  inventory, safe-command policy, privacy and generated-content rules; read
  before any substantial survey.
- `references/workflow-write.md` — choosing and shaping the doc set; read when
  authoring more than a single file.
- `references/workflow-refresh.md` — the two-directional audit and the drift
  report; read when refreshing existing docs.
- `references/document-shapes.md` — shapes and table formats for files that have
  earned existence; read at drafting time.
- `references/human-register.md` — the generated-text patterns human-facing
  prose must not carry; read before drafting or materially revising
  human-facing prose.
- `references/parallel-sets.md` — the parallel `docs/` and `agent-docs/`
  trees: layout, frontmatter, generated indexes, agent-primary derivation,
  and the chained accuracy review; read whenever the project keeps or earns
  a `docs/` tree.

