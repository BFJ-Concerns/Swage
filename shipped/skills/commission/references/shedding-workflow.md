# The Shedding Run (Update Path)

The README grows under capture and shrinks under one force only: shedding. If
shedding is something a session might remember to do, the file bloats — so it is
a standard beat of every update, not an opportunistic afterthought. You are
already in the contract changing things; sweeping for what the build has since
made redundant is part of the same job.

A shedding run is four beats — **sweep, ground, shed, preserve** — and the
machinery scales with what the sweep finds. Most small updates shed nothing and
pay nothing: the always-on part is the cheap question, not the fan-out.
Greenfield capture runs none of this — nothing is built yet, so nothing is
sheddable.

This skill's invocation authorises the run these beats call for — the grounding
investigation and the two preservation passes — and nothing wider.

## Beat 1 — Sweep (what could be shed?)

List the candidates: features the README still spells out in full that the build
may now embody — anything this delta touches, plus anything plausibly finished
since the last commission. This is judgement, not a dispatch, and it is always
on during an update. An empty list is a fine answer; an unasked question is not.

## Beat 2 — Ground (is it actually built?)

Shedding's premise — "the code now documents this, so the README can stop" — is
a claim about the world, checked before anything is compressed. The check is
the carried cross-family `grounding` workflow
([ensemble-workflows.md](ensemble-workflows.md)), not the main session's own
read: it needs the perspective the main session cannot give — blind to this
session's recollection of what was built, and, for the verdict that deletes
intent, confirmed by a different model family. Feed it the Beat 1 candidates in
observable terms and the location to investigate — the code repo, or the
orchestrator's run state where that is the better record of what was built. Each
candidate comes back:

- **built and cross-family confirmed** → it sheds.
- **not built, partial, or unconfirmed** → it stays in the README, fully
  elaborated, and is reported.

The workflow is conservative by construction — a candidate sheds only on a
positive find *and* an independent confirmation, because a wrong "built" deletes
a feature that was never built and nothing downstream re-checks that verdict.

This is the one place the skill reads build state, and it reads it to decide a
cut, never to write progress into the contract. *Destination, never journey*
holds: the answer gates an edit, it does not enter the file. The output is a
clean cut/keep list — what goes and what stays — and the `cut_keep_list` the
preservation passes (Beat 4) take as input.

## Beat 3 — Shed (compress and relocate)

For each item that sheds: compress it in the README to a sentence — the code is
now its own documentation — and move the full detail to `shed/`, one dated file
per shed feature, under the mandatory header (shed date; historical record, not
current intent; the README wins). A commitment line is exempt from the
compression: it is already the compressed form, so it stays verbatim — ID,
consumer, act, falsifier, attributes — and what sheds is the elaboration
beneath it (a line leaves the register only by descoping, never by a shedding
run). Detail is relocated, not destroyed:
recoverable when a wrong implementation needs the spec it was built against.

Shedding compresses the What; it never quietly takes a Decisions entry — or a
commitment line, or the reader-protocol block — with it.
If the rewrite would delete or weaken a Decision, that is a retirement — the
SKILL.md retirement rule applies (settled with the user as its own item,
appended to `shed/decisions.md`), and the retirement joins the
deliberate-changes record so the preservation passes can tell a signed
retirement from a silent drop.

## Beat 4 — Preserve (did the rewrite lose anything — or gain anything unsettled?)

A shed rewrite can lose intent the way any rewrite can — a constraint compressed
out, a boundary dropped with the paragraph around it, detail that was meant for
`shed/` but never landed there. And it can gain intent the same way: a normative
sentence authored into the rewrite prose that no conversational exchange ever
settled — invisible inside a large legitimate diff, and implemented faithfully
by a later build. The preservation passes check both directions. They are the
after-check, run before the sign-off read, and they fire whenever this run
actually shed something or materially rewrote existing intent — not for a
one-line addition settled item-by-item through the handshake, which the per-write
collision read already covered.

The two passes run as the carried cross-family `preservation` workflow
([ensemble-workflows.md](ensemble-workflows.md)); their opposite information
diets — pass 1 blind to what changed, pass 2 sighted on the record — are the
whole point and are enforced by what the workflow is fed. Build its brief:

- **baseline** — the README at `HEAD` (`git show HEAD:README.md`), the contract
  as last published.
- **candidate** — the working-tree draft *plus* this run's new `shed/` files:
  detail moved to `shed/` has relocated, not left, so the comparison must see
  both or every legitimate shed reads as a loss.
- **deliberate_changes** — the integrated delta, the cut/keep list from Beat 2,
  what was shed and the `shed/` file it went to, what was deliberately dropped
  (with the Decision recorded for it, where one was met), and `settled_items` —
  each item this session settled through the handshake, as the user confirmed
  it. The workflow shows this to the adjudication pass only; the blind scan
  never sees it.

The workflow returns the blind findings and, per adjudicating family, an
adjudication of each — confirmed loss, accounted-for relocation, justified
removal, rejected, or insufficient evidence. It also returns the gains
direction: the blind pass extracts every new normative claim the candidate
makes (judged by builder effect — what an implementer would do differently
because the sentence exists), and the adjudication sorts each into settled or
unsettled addition against `settled_items`, checking every claim — settled ones
included — against the standing contract for collisions. The adjudication pass
runs even when the blind scan is clean: with the record in view it sweeps for
material losses the blind pass missed, returned as `additional_issues`.

## Triage And Sign-off

Surface both the blind findings and the adjudication to the user in chat — never
collapse to a pass. Confirmed losses — and the sighted sweep's
`additional_issues`, material losses by construction — are reinstated into the
draft before the sign-off read, so the diff the user signs already includes the
reinstatement. An **unsettled addition** is the mirror case with the mirror
remedy: put the claim to the user through the handshake — it becomes a settled
item, or it comes out of the draft; it never rides into the sign-off read as
prose. A **collision** goes to the user with both quoted texts — the claim and
the standing rule it pulls against — for the user to reconcile; the contract
never signs carrying both sides of a contradiction the review surfaced.
Disputed or multi-option findings go to the user with the reviewer's complaint,
not silently resolved either way — the user-present handshake is the filter that
belongs here. Triage, don't obey: a sceptical reviewer drifting stricter across
reruns is noise, so stop rather than loop.

The run's convergence joins the Stop condition in `SKILL.md`: an update is not
done while a confirmed loss, an unsettled addition, or an unreconciled collision
is unresolved, or while the passes have not run where a shed or material rewrite
fired them.
