# Ensemble Workflows

Commission ships six workflow scripts in `workflows/`. Each entry below gives what the
script is for, when to fire it, the information diet it needs, and how to read what it
returns. The runtime that runs them — how to launch (*Running a workflow* below), the hooks,
the schema rules, the live-engine requirement, and run-tuning — is owned by the
`ensemble-workflow` skill; read it there before running or editing a workflow.

The six, all cross-engine so a *different* model family checks the work:

- `workflows/divergent-survey.js` — generate genuinely different approaches blind
  to the anchor, cross-critique, surface a *ranked* comparison for joint decision.
- `workflows/blind-rederivation.js` — re-derive a whole design from first
  principles, isolated from the incumbent, then compare blind against settled
  for convergence and divergence.
- `workflows/draft-review.js` — attack the near-final draft sighted on the settled
  Decisions, returning findings grouped for the reconciliation beat.
- `workflows/grounding.js` — the shedding run's blind build-state check: find which
  candidate features are actually built, then cross-engine verify the ones that
  would shed before they do.
- `workflows/commitment-audit.js` — the pre-sign-off register audit: both families
  mine the conversation's transcript for voiced wants, a blind diff returns the
  wants no commitment covers, and whole-register adversaries record per-line
  hollow verdicts.
- `workflows/preservation.js` — the two-pass preservation review, both directions
  (lost intent and unsettled new claims): blind scan, then sighted adjudication.

The survey, re-derivation, draft review, and preservation are *divergent or
adversarial* moves; grounding and the commitment audit are *verification* — but
they earn the cross-engine ensemble for the same reason the others do: the
verdict that sheds a feature deletes intent, and a register audited only by one
family inherits that family's blind spots — in each case the strongest check is
a different model family.

Every script declares its worker policy once in an `export const defaults`
block — per-engine model tier, effort level, and the Claude fallback tier —
which every `agent()` call inherits; call sites override only where a stage's
residue earns a different level, with a comment naming it. The policy is not
clutter: without it a call runs whatever the engine's ambient default model
and level are that week, so cost and character drift with lineup changes far
outside the run. `fallbackModel` (Claude engine only — other engines reject
it) declares the adjacent tier to use while the pinned one is unavailable,
and the run archive records when it fires; the runtime also warns on any
Claude call that reaches it unpinned — Codex calls get no such warning, so
the defaults block is the only guard on that engine. The doctrine behind the
pins is the shared pair — [model-selection.md](model-selection.md) for the tier,
[effort-calibration.md](effort-calibration.md) for the level. Keep the policy
blocks and overrides intact when editing a workflow.

## Hard requirement — both engines, live

Every workflow here is cross-engine by design. The live requirement — both model
families reachable in one run plus `tmux` for the Claude reply valve — and how to
respond when one is unreachable are owned by the `ensemble-workflow` skill's
*Live requirement* section; read it before running a move. What may run in a
downed move's place — and what may not — is this skill's own doctrine: the Hard
requirement block in `SKILL.md` (stop and tell the user; a clearly-labelled
degraded review only where that block allows one, decided together).

When the user chooses a degraded review — available for the draft attack,
the preservation passes, and the commitment audit only — it runs under this contract: use the
corresponding workflow script's input and output schemas as the checklist,
producing every field, marking the cross-engine ones as not performed, and
reporting any field that could not be produced; label the result plainly as a
degraded review that shares this model's blind spots; and for preservation,
run the blind pass in a fresh context that has not seen this session's draft
or diff — the main session authored the change, so it cannot self-blind.
For the commitment audit, a degraded run executes mining, diff, and hollow
legs on the one live family, labels every product one-family (it shares that
family's blind spots), and is recorded as degraded in the sign-off report and
the Stop condition — the user's explicit say-so is what lets a sign-off
proceed on it.
Re-run the real move once the ensemble is back if the user wants the full
gate. The survey and grounding have no degraded rendering: the survey's value
is the other family's divergence, and a shed happens only on the live
cross-engine confirm. What is specific to
commission: a round that touches no product intent triggers none of these
moves — but any round that works the What owes the commitment audit before
its sign-off read, so capture and feature updates need both engines live by
the time the audit runs, and an update
that will shed needs them before Beat 2 as well.

<!-- foundry:engine-placeholders ensemble-runtime start -->

## Running a workflow

The runtime lives in the `ensemble-workflow` skill — a self-contained Node ≥24 launcher,
nothing to install. Run a workflow with flags **before** the script path; the result is
one line of JSON on stdout, logs on stderr:

```bash
node ~/.claude/skills/ensemble-workflow/scripts/ensemble.mjs --json-args '<json>' ~/.claude/skills/<skill>/workflows/<name>.js > <scratch>/<name>.json 2> <scratch>/<name>.log
```

`<skill>` is the skill this reference ships with — substitute its bundled workflow
script's installed path (a skill that documents a concrete command already spells it
out). Both paths are absolute so the command runs from any
cwd — launch from the directory the move itself calls for (a blind stage from an empty
scratch directory, a grounded one from the code repo), never from a directory chosen just
to make a relative path resolve.

The script **cannot touch the filesystem** (sandboxed): pass every input in `--json-args`,
and **you are the driver** — write the returned stdout to `<scratch>`, read it, and bring
the result back. Read each workflow script's header for the inputs it names.
Everything else — the flags (`--budget`, `--concurrency`, `--timeout`), their defaults and
when to set them, the live-engine requirement, and the fan-in limits — is owned by the
`ensemble-workflow` skill; read it before running or authoring a workflow.

<!-- foundry:engine-placeholders ensemble-runtime end -->

## divergent-survey — blind, for joint decision

Run this when an important option space has not been walked, or when the user has
offered an approach where an outcome would do (anchor risk). A research stage
surveys the published theory of the outcome first; half the proposers are seeded
with disjoint slices of it, half work from the brief alone.

- **Keep the proposers blind to the anchor.** This is the whole point — a
  proposer that sees the user's named approach rediscovers it and the survey
  becomes theatre. Two controls, both required: the **brief** you pass states the
  *outcome* in observable terms and the hard *external* constraints only — never
  the proposed approach, its vocabulary, or anything that betrays the expected
  answer; and because workers have filesystem access, **launch from an isolated
  empty directory** under `<scratch>` (not the project root, whose code or notes
  could re-anchor a worker). The anchor-free brief is the real control; the empty
  cwd is the backstop that removes the breadcrumbs.
- **The research stage widens the pool; it does not anchor it.** Two Codex researchers
  (the Codex backend provides live web search) survey the outcome's published
  theory with complementary questions — established techniques, and documented failure
  modes. Their findings are dealt in disjoint slices to half the proposers; the rest
  propose from the brief alone, so the pool carries both grounded prior art and
  unanchored invention, and the cross-critique arbitrates. The researchers receive the
  same anchor-free brief as every other worker — they survey what is established for
  outcomes of this kind, never the incumbent. A researcher independently surfacing the
  same public tool the user named is convergence signal, not an anchor leak. Carry the
  returned `research` digest into the joint discussion. Findings carry their `source`,
  with "parametric" marking model knowledge rather than a document — weight the two
  accordingly. An empty digest means the survey ran without theory grounding
  (researchers collapsed, or nothing established was found — the run log distinguishes
  them): say so when relaying, rather than presenting the approaches as
  research-checked.
- This is a **creative** stage for the *shape* of an approach — do not constrain the
  shape with citations; that degrades ideation. But the trade-off **claims** each
  approach makes (what it makes easier/harder, when it wins, its evidence) still
  follow the grounding rules — evidence attached, "couldn't determine" welcome,
  observation kept separate from inference. Further rigour lives in the critique
  (which checks divergence and grounding) and in your later reconciliation against
  the Decisions.
- Pass the `brief` (outcome, constraints, importance) the script header
  documents. Building the anchor-free brief — stripping the user's *how* back to
  the *what* it serves — is your job before you invoke.
- **When re-surveying a part of an existing commission, brief the part's whole
  outcome, not the delta.** State the outcome as it should stand after the
  change, with the behaviour that must survive written in as outcome
  requirements — never "the outcome now also needs X", which makes proposers
  patch the unseen incumbent instead of proposing whole approaches against
  which the incumbent is judged as one alternative. Delta scoping is the
  exception, only when the user has explicitly ruled the part's overall
  approach out of bounds.
- Read the returned `research`, `approaches`, `critiques`, and the two `syntheses`. Relay the
  **ranked options** with their evidence and the **decisive questions** to the
  user, and decide together. When the survey was run on an approach already on the
  table (the derivation case), read convergence and divergence as signals: strong
  convergence is quiet confirmation the proposed route may be right; wide divergence
  is a finding either way — sometimes the survey's approach is better, sometimes the
  user's is and the divergence shows where the README under-explains the *Why*.
  **Never silently adopt the top-ranked option** — the ranking sharpens
  the choice, it does not make it. The `strongest_elements` are ideas worth
  grafting into whichever route is chosen — carry them into the discussion
  rather than discarding them with the losing options.

## blind-rederivation — a whole design from first principles

Run this when the subject is a design taken whole — a greenfield counterpoint once
the user's intent is extracted, a major new feature, or a settled area being
revisited (including one suspected of a wrong starting place) — rather than one
open decision (the survey's territory).
One sustained top-tier designer per chunk derives the concerns and the shape from
the brief alone; when a settled design is supplied, both families independently
compare blind against settled.

- **Isolation is by construction — launch from an isolated empty directory** under
  `<scratch>`, outside the project: no repo, no README, no notes for the designer
  to find. The workflow shows the settled design only to the comparison stage,
  never to a designer.
- **The brief is the other half, and auditing it is your job before you invoke.**
  For each chunk: the domain stated generically; every product-specific noun,
  agent name, and taxonomy from the incumbent stripped; every requirement worded
  as a problem to solve, never a shape to build ("users create reusable material —
  what the natural units of authorship are is yours to derive", not the incumbent's
  four-kind taxonomy); and the user's core hypothesis, where one exists, passed as
  `premise` so the designer builds on it rather than relitigating it. The
  technique is only as good as the brief is clean: before invoking, re-read the
  brief hunting for anything that betrays the incumbent's answer.
- **Chunk by coherence, not by fan-out.** A feature-sized subject is one chunk. A
  project-sized one is split into pieces a designer can hold coherently without
  the rest — run in sequence, each later chunk handed the earlier blind output as
  its fixed consumer contract with explicit licence to propose amendments in a
  dedicated section. Do not split a subject that one designer can hold, and do
  not force chunks to be independent when they are not — staged dependence with
  amendment licence is the designed-for shape.
- **Omit `existing` for the greenfield counterpoint case** — there the comparison
  is the conversation itself: the user's extracted intent on one side, the blind
  derivation on the other, and you walk the convergences and divergences with
  them live.
- Read the returned `designs` (each entry indexes a document in the run directory
  — read the documents themselves, they are the deliverable) and `comparisons`.
  Relay convergence as independent validation of the settled choice and each
  divergence with its `underlying_assumption` — the question one design asked and
  the other never did; that assumption, not the surface disagreement, is the
  finding. Proposed amendments to earlier chunks are settled explicitly with the
  user, never silently absorbed. Nothing here decides: divergences and the
  designers' load-bearing choices go through the reconciliation beat and the
  handshake like any other finding.

## draft-review — sighted on the Decisions, near sign-off

Run this shortly before the sign-off read, scaled by the size of the change
(standard for greenfield capture, skippable for a one-line update).

- **Feed it the settled Decisions**, each tagged `examined` or `provisional`,
  alongside the full draft and what changed this session. This is what stops the
  reviewer manufacturing phantom gaps from already-decided questions.
- The default lenses cover contradiction, underspecification, feasibility,
  traceability, and overall shape. Pass `lenses` only when the draft has a
  genuinely different expert surface to attack — not invented specialists
  rereading the same draft from generic angles.
- Read the merged `reviews`. Each raw entry in `findings` carries the lens and
  engine that produced it — that is where to look when judging how a single lens
  performed. Apply the **reconciliation beat** (`SKILL.md`): the
  `material_findings` are candidate fixes; the `reopen_prompts` are challenges to
  settled Decisions framed as *questions* — take them to the user, never apply
  them as silent reversals, and reopen an examined Decision only on the genuine
  new information the prompt names; `traceability_gaps` are settled Decisions the
  draft dropped or weakened, to reinstate. The held-findings count is logged, not
  dumped.

## grounding — blind build-state check, for the shedding cut/keep

Run this for the shedding run's Beat 2 ([shedding-workflow.md](shedding-workflow.md)),
whenever the sweep finds candidates the build may now embody.

- **The blindness is the inverse of the survey's.** The survey hides the repo from
  its proposers (empty cwd) so they cannot rediscover the incumbent; grounding's
  workers *must* read the real code, so **launch it from the code repo** (or hand
  the brief the path to investigate), not from an empty directory. What stays blind
  is the *expectation*: each candidate is described in observable terms and the
  worker is asked what the code actually does — never "the README says X is built,
  confirm it", which would buy back the anchor the dispatch exists to escape.
- **Find is load-balanced; only the deletions are verified cross-engine.** Stage 1
  fans the candidates across both families and judges each built / partial /
  not-built / unknown. Stage 2 spends a cross-engine check only on the ones judged
  *built* — the shed direction — where the opposite family independently hunts for
  why the "built" call is wrong. The asymmetry is deliberate: a false "not built"
  merely keeps a feature that could have been compressed (cheap, self-correcting);
  a false "built" sheds a feature that was never built, and nothing downstream
  re-checks it (preservation reads the relocated detail as an accounted-for move).
- Pass the `repo` (the location to investigate) and the `candidates` (the Beat 1
  sweep, each with an observable `feature` description) the script header documents.
- Read the returned `cut_keep_list`. A candidate sheds only when the finder judged
  it built *and* the cross-engine verifier confirmed; everything else keeps. Shed
  the confirmed set, keep and report the rest, and surface any finder/verifier
  disagreement to the user rather than resolving it silently. The list is also the
  `cut_keep_list` the preservation workflow takes as input.

## commitment-audit — recorded register gates, before the sign-off read

Run this before the sign-off read of any round that worked product intent —
capture, a feature delta, a hold or release, elaboration edits — whether or
not a commitment line changed: the exclusion diff exists to catch wants
voiced but never written, which is precisely a round that changed no line.
Only a round that discussed no product intent (a pure queue triage, a config
knob) skips it. Run it late, once the register has settled. It
converts three capture disciplines from in-room diligence into recorded
verdicts, so a skipped or failed check is visible.

- **The miners read the transcript, never a summary.** Pass the absolute paths
  of this conversation's own transcript files, located per SKILL.md's Session
  Transcripts — every file the conversation spans, since a resumed or
  compacted session splits across several. A summary of the conversation
  defeats the audit by construction: the want the summary dropped is the want
  the audit exists to find.
- **The diff is blind by construction.** Each family's differ receives the two
  bare lists — mined wants, commitment lines — and nothing else. Do not add
  register context or your own matching opinions to the input.
- Pass the `transcripts`, the bare `commitments` (`id` + full line text — the
  whole draft register, which the diff needs for coverage), and `hollow_ids`
  scoping the hollow stage to the lines created or changed this round plus
  any line never yet audited — omit it only when the whole register is new.
  Launch cwd is unconstrained — workers read the
  transcript paths directly.
- **A missing leg is a missing check, never a pass.** The output's
  `mining_gaps` and `diff_gaps` name every (file, family) leg that returned
  nothing, every transcript a miner could not fully read, and every leg that
  twice returned zero wants (the workflow retries a zero-want leg once before
  recording it — an empty pool from a commission conversation is presumed a
  mining failure, so confirm with the user that the conversation genuinely
  voiced nothing before reading the diff as complete); while either is
  non-empty the exclusion diff is not the two-family union it claims to be —
  re-run the failed legs, or treat the audit as degraded under the Hard
  requirement above.
- Read back three products, each acted on before the page publishes. The
  **exclusion diff** (union of both families, deduplicated by you) carries only
  quoted human wants — a differ's remarks about the matching question itself
  arrive separately in `diff_concerns`, to read but never adjudicate as
  wants — and is settled
  want by want with the user — a new commitment, a hold, a non-binding tail
  mention or `IDEAS.md` entry, or a deliberate
  exclusion, and the deliberate exclusions become sign-off decision cards. The
  **hollow verdicts** name the cheapest implementation that survives the whole
  register, judged per line; a hollow verdict from either family is closed by
  one of three dispositions — the line sharpened through the handshake until
  the cheap reading fails its falsifier; the verdict recorded as covered by
  siblings, where the register's other lines already answer the complaint
  (its `covered_by_siblings` field, or your own whole-register read, names
  them); or the verdict accepted as a residual, where sharpening would demand
  over-specification the surprise test forbids, carried onto the sign-off page
  with its reason and closed by signature. A line whose text actually changed
  in response to a finding gets one fresh verdict before signature — at most
  one re-run per line per round — and a `null` verdict
  (worker collapse) is a
  check still owed, never a pass. The **mined wants** are the evidence base —
  consult them when a diff entry needs its conversational context. Read every
  `out_of_scope_concerns` before trusting the verdict beside it.

## preservation — blind scan, then sighted adjudication

Run this on the update path whenever a shedding run actually shed something or
materially rewrote existing intent (see [shedding-workflow.md](shedding-workflow.md)).

- Pass the `baseline` (README at `HEAD`), the `candidate` (working-tree draft),
  the run's new `shed/` files (part of the candidate), and the
  `deliberate_changes` record — including `settled_items`, each item this
  session settled through the handshake as the user confirmed it, which is what
  the gains direction adjudicates extracted claims against. The workflow shows
  the record only to pass 2; pass 1 stays blind by construction.
- Read the `adjudications`. **Reinstate every `confirmed-loss`** into the draft
  before the sign-off read; take disputed or multi-option findings to the user
  with the reviewer's complaint. Surface both the blind findings and the
  adjudication to the user — never collapse them to a pass.
- Read the `claim_adjudications` — the gains direction. An
  `unsettled-addition` is a rule in the draft no exchange ever settled: put it
  to the user through the handshake — it becomes a settled item or comes out of
  the draft, never signs through as prose. A non-empty `collision` — on any
  verdict, settled additions included — goes to the user with both quoted
  texts to reconcile. A claim both families extracted is one claim
  (convergence); a claim only one family extracted is still adjudicated, not
  discounted.
- Pass 2 runs even when the blind scan is clean: with the record in view it
  sweeps for material losses the blind pass missed, returned per adjudicator as
  `additional_issues`. Weight each adjudication verdict by its `confidence` and
  start its reinstatement from its `recommended_action`; an `additional_issue`
  carries neither field — treat it as a confirmed loss (reinstate it, or take
  it to the user) unless evidence already in hand rejects it.
- The two adjudicating families report independently; reconcile them
  conservatively. Union their `additional_issues`; a credible `confirmed-loss`
  from either family stays unresolved until reinstated or taken to the user;
  surface cross-engine disagreements rather than resolving them silently —
  `confidence` prioritises work, it never erases a verdict.
