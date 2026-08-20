# Refresh Mode

Bringing existing documentation up to scratch. The deliverable is a drift report
plus targeted patches — not a rewrite.

## Scoping

- **Narrow** — after commits, a PR, or specific feature work: read the commits or
  diff, group the changes into logical areas, and note for each the relevant
  files, what changed, and which existing docs may be affected. Only docs
  affected by the changes are touched.
- **Broad** — a full-repo audit, first documentation, or a major refactor:
  explore the codebase structure, identify the major functional areas, and audit
  all documentation for each.

Area criteria, either way: each area is a cohesive unit of functionality; areas
map to user-facing concepts where possible; 3–8 areas suit a typical project,
more for a large codebase; one area per distinct interface type (API, CLI,
config) is often sensible.

## The Two-Directional Audit

Auditing only the existing claims misses half the drift. Run both directions:

1. **Docs → code.** Classify every substantive claim: current, stale, false,
   unsupported, duplicated, or misplaced. Evidence rules per
   `survey-protocol.md`.
2. **Code → docs.** Every implemented user-facing surface either has a
   documented home or is a gap.

## The Drift Report

Written **before** editing — it is the patch plan the user can sanity-check — and
delivered in conversation or the PR description, never committed to the repo.
Material mismatches only; three categories:

- **Misdescription** — the doc says X, the code does Y; cite both.
- **Gap** — implemented and reader-relevant, but undocumented.
- **Dead doc** — documents code that no longer exists.
- **Misplaced** — accurate but out-of-documentation-scope content
  (development-process or planning material); name it, its location, and its
  disposition — stripped, relocated, or surfaced for the planning layer or
  the agent-guidance hand-off.

## Parallel Sets

Where the project keeps the parallel trees (`parallel-sets.md`), the audit
covers both and the patching follows precedence: bring the affected
`agent-docs/` pages true against the code first, then re-derive the human
pages that restate them; human-only pages patch directly. Finish by
regenerating the indexes (`scripts/doc-indexes.sh`) — a page added, removed,
or retitled without its index entry fails the verify step's check.

## Patching

- The unit of change is the wrong claim: smallest clear semantic edit, correct
  prose left untouched, voice and heading style preserved where accurate.
- A whole-file rewrite needs a declared reason in the drift report first —
  "targeted patches cannot leave this file clear" — and a mostly-aspirational
  doc is a legitimate such reason.
- **Drifted duplicates**: the code arbitrates which copy is true; the audience
  rule (who acts on the fact) picks the surviving canonical home; losing copies
  become signposts; inbound links are repaired in the same pass.
- **Dead docs are deleted**, noted in the drift report — a doc describing removed
  code misleads worse than a gap.
- **Planning content found in docs** — roadmaps, task lists, wishlists — is
  relocated to the project's planning layer or surfaced in the report, never
  silently dropped and never left in the code repo's docs.
- **Development-process content found in docs** — contribution setup,
  development workflow, release mechanics, architecture and decision records —
  is out of documentation scope (the skill's Scope): within the files the
  job's scope covers, delete it, recording each strip as a Misplaced entry in
  the drift report and surfacing anything the planning layer or the
  agent-guidance hand-off should keep. It is never left in place — but the
  strip stays inside the docs trees: root-level community and legal files are
  outside this skill's authority (the skill's Scope), and out-of-scope files
  where such material is noticed are report material, not targets.

## Verifying the Refresh

The accuracy review covers every claim added or changed. For a very large doc set
where unchanged claims would double the cost, sampling the unchanged ones is
acceptable — state the coverage in the report rather than implying a full pass.
