# Briefs, Restatements, Reports, PR Bodies

The document shapes a unit produces. Dense prose under plain headings,
sized to the unit; never padded to look complete. Read when writing the
first brief of a run.

## The Brief (`build/units/<slug>/brief.md`, write-once, before dispatch)

Before an obligation's first brief, answer fan-out then provability
(SKILL.md, *Sizing at Dispatch*); fan-out decides how many briefs you
are writing.

A brief states **what done looks like and why it matters — never how to
get there**; the builder owns the how. Two lines record decisions the
admission preflight (SKILL.md, Admission Is Binary) already made, and
the builder re-judges them at Restate as a backstop:

- **Obligation:** the obligation record this unit serves, by slug. Mandatory
  for every brief.
- **Extends:** the spine exemplar (by its `build/spine.md` name) this work
  extends. Mandatory except for the estate and docs cases. An obligation no
  exemplar covers never reaches a brief — it
  went `needs-spine` at the preflight (SKILL.md, Admission Is Binary). Estate
  briefs are the one exception (SKILL.md): they name their obligation and
  ladder rung, and carry `Extends:` only when a test-pattern exemplar
  exists to name.

The rest carries, in whatever order serves the unit:

- **Outcome** — the destination, verifiable, including wired-in-and-working
  expectations within the unit's scope. Sized by provability: one consumer
  proof covers it (SKILL.md, Sizing at Dispatch).
- **Proof expectation** — how the obligation's proof will run, from its
  obligation record: the journey or command, the consumer exercised, and for
  joins the producer, final consumer, and the distinctive evidence the
  proof must observe there — a stub answering success proves only the
  caller's handling of success. Name the discriminating case — what
  the proof must observe that a tree lacking the behaviour cannot
  show; a proof the defect satisfies proves nothing. The expectation names the obligation's
  own journey, never a project gate: gate venues are
  fixed (references/landing.md, *Gate Venues*), and a gate written into
  the expectation re-runs at every stage that honours it. The named
  proof is bound by the project's ladder for the surface the unit
  touches, as the ladder stands plus any rung readings settled this
  run: a unit whose claim is narrower than its obligation's licenses a
  smaller proof at the same rung, never a lower one, and selecting
  below the ladder is a degradation the brief declares, not a silent
  choice. A rung reading settled by a ruling belongs in the ladder file
  itself — route it there through `build/gaps.md` for the commission
  pass, and honour it in every later brief meanwhile: a reading left in
  a gaps entry is one no brief consults.
- **Intent** — the why, lifted from the commission: the builder's
  generalisation engine for decisions the brief never anticipated.
- **Constraints** — external facts the builder cannot invent; the relevant
  slice only.
- **Inline non-goals** — exactly where a capable builder generalising from
  the intent would plausibly build the wrong thing, each with its why. The
  exemplar's imitation boundary (from its spine entry) is restated here
  when the unit works near it.
- **Changelog expectation** — when the repository keeps a changelog, the
  outcome includes an entry in its convention, in the same change as the
  user-visible behaviour; a unit whose work is internal-only says so here.
- **The situational slice** — what was agreed, tried, abandoned; workers
  share skills and house rules, never memory.

A brief that presupposes an existing surface — a wiring target, a
path to extend, a call to make — rests on a glance that the surface is
really there: a grep or read at your seat while writing, seconds, never
a dispatched stage or a proof obligation. The test is "the surface
exists, or its creation is chartered into this unit's scope or into a
named serialised sibling under the same obligation" — so a
brief that builds the surface alongside its consumer passes by the
charter, as does one whose named sibling builds it, and the glance
never refuses either. A glance that finds neither routes the brief
back: charter the surface into scope, re-scope the unit, or file the
contract question — the brief is not dispatched while the surface is
neither present nor chartered. What the glance exists to stop is a
builder briefed to wire into machinery nobody can reach, discovered
only when it refuses to fabricate.

Every path absolute. Briefs for reopened work state the whole intended
outcome and point at the incumbent unit's report — a delta-scoped brief
makes the builder patch an incumbent it has never seen.

## Sibling Briefs (one obligation, parallel units)

When an obligation dispatches as parallel slices (SKILL.md, Sizing at
Dispatch),
each sibling is a full unit with its own slug — the obligation slug plus a
slice suffix (so `build/units/` directories and the ledger's `in-flight`
list stay unambiguous) — its own complete brief, its own `Extends:` line,
and its own worktree, all sharing one `Obligation:` slug. Two additions
keep the siblings apart:

- **Outcome** states the slice's own boundary: what this unit builds, and
  the seam where the neighbouring slices' work begins.
- **The situational slice** names the sibling units and what each is
  building, so an unfinished neighbouring seam reads as in-progress work
  to interface with.

Keep one proof on the obligation record and run it over the joined work — the
tree containing every sibling: main under direct-merge; in PR mode a
composed ref (the structural branch plus the sibling branches, named in
the evidence), since each WIP PR holds until its obligation is proven. Each
sibling's proof expectation names the slice's contribution to that joint
proof, not a substitute for it; running the joint proof is yours, after
the last sibling lands, its evidence recorded on the obligation.

## The Restatement

The pipeline's first hold. The builder restates the brief in its own words
and sketches its approach; judge the sketch, not the paraphrase — an
accurate paraphrase verifies nothing, and the sketch is where
misunderstanding surfaces. Correct with a sentence, require a fresh
restatement of the changed part, accept when projections stop producing
corrections. A restatement disputing the brief (the exemplar does not
cover this; the obligation is already true) is a finding to check, not a
deviation to overrule.

## The Report (`build/units/<slug>/report.md`, part of done, always)

The journey the diff cannot show: what was built at one level above the
diff; what was tried and abandoned, and why; what fought back; how the
builder verified within its scope and — named just as plainly — what it
did not exercise; the check gate's recorded run — the exact command, the
tree sha it ran on, and the result, alongside the touched-surface test
runs (references/landing.md, *Gate
Venues*), with a gate the builder could not get green reported as a
failure finding (what failed, what was tried), never as a quiet result
line; a proof expectation the unit could not meet honestly at its rung,
and a fix disposition left unapplied as blocked, reported as failure
findings the same way; an overall confidence (high, medium, low) with a
one-line why. Findings that smell like product ideas or contract gaps go
here for routing — builders never write to the annexe root. In PR mode
the PR body is rewritten from the report, never the reverse.

## Verifier Findings

Verifier stages report to one contract — work shown, verdict withheld:
what was examined and exercised, and what could not be seen; findings each
with severity and confidence, never self-filtered; concerns considered and
rejected; overall confidence bounded by the unseen list. Every command a
verdict leans on is evidenced by the command line as executed plus enough
of its actual output to support the claim — or an explicit "could not
run: <reason>"; a run cited with neither is an unevidenced claim, not a
completed check. Where the
exercise drives a tool that writes outside the unit's worktree (an
installer, a deployer, anything with absolute destinations), isolation is
evidence, never intention: throwaway destination roots with the
redirection mechanism named, and the report states what proves isolation
held — a check that cannot be isolated does not run against live
destinations; it comes back as a finding that the proof is unavailable. An empty
examined-account invalidates the report — the pipeline re-runs the stage
rather than reading it as clean. Read builder and verifier confidence as a
pair: divergence marks where to dig first.

## The PR Body (`build/units/<slug>/pr.md`, PR mode only)

Self-contained for a reader with no annexe access, written to be checked
rather than trusted: **What changed** (one level above the diff), **Why**
(intent, distilled), **What must be true** (the obligation's proof
restated as checkable claims), **Known gaps** (what was not exercised;
omit when none). No verification narrative and no steer toward where to
look — both hand the reviewer the author's framing. Preliminary at PR
open, rewritten whole from the report at landing.
