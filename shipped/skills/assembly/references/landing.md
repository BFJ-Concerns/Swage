# Landing and the WIP Latch

The full landing discipline per merge policy. Read at the first landing of
a run and at every de-WIP, re-block, or run-close PR decision after that.

Landing stays inside the pipeline (its landing stage merges under
direct-merge, or pushes and opens the `WIP:`-prefixed PR under PR mode —
docs units excepted: they merge direct under either policy, *Docs Units
Land Direct* below) and
follows the annexe `config.toml` knobs (`merge_policy`, `push_policy`).
Everything after the pipeline's landing stage is yours:

## The Forge Route

Every PR act below — opening, reading back, de-WIPing, re-blocking,
closing, wiring dependencies — runs against the forge the project's
primary repository lives on, and the route is read, never assumed. The
annexe's `config.toml` names it as `forge.primary = "<forge>:<owner>/<repo>"`;
where that key is absent, read `git remote get-url origin` in the code repo:
a `github.com` remote is forge `github`, and any other host is a Forgejo
instance whose alias is that host (the alias keys a section of
`~/.config/forgejo/instances.toml` carrying `url` and `token`). The
`reviewers` list in the same file names the automated reviewers a head must
pass; how each reports is that reviewer's own contract.

- **github** — act through `gh`, always with `--repo <owner>/<repo>`:
  open with `gh pr create --base <base> --head <branch> --title "WIP: …"
  --body-file <pr.md>`; read back with `gh pr view <n> --json
  number,title,body,headRefName,baseRefName,state,mergedAt,mergeCommit,url`;
  de-WIP and re-block with `gh pr edit <n> --title …`; close unmerged
  with `gh pr close <n> --comment "<reason>"`; switch on
  delete-branch-on-merge with `gh repo edit --delete-branch-on-merge`.
  GitHub has no PR-dependency field: an ordering that targeting cannot
  express is stated in the PR body (`Depends on #n`) and checked by
  reading the dependency's state before landing. Authentication is `gh`'s
  own; a failure is reported as `forge auth`, never worked around.
- **a Forgejo alias** — act through the instance's API at `<url>/api/v1`:
  `POST /repos/<owner>/<repo>/pulls` to open, `GET /repos/<owner>/<repo>/pulls/<n>`
  to read back, `PATCH` the same path to retitle or close, and the
  repository settings endpoint for delete-branch-on-merge. The token is
  read into the request at execution — `-H "Authorization: token $(cat …)"`
  or a variable loaded the same way — never printed into a transcript or
  pasted as a literal. Forgejo carries PR dependencies natively; wire and
  read them back there.

On either forge the branch is pushed as a tracked branch (`git push -u
origin <branch>`) before the PR is opened; `refs/for/*` (the AGit flow) is
forbidden everywhere — it leaves no tracked branch and cannot carry a
multiline body.

## PR-Mode Topology

Under PR mode, the handover ref is the structural branch, and it is the
build's integration lane until promotion:

- **Unit branches are cut from the structural branch tip**, and unit PRs
  target the structural branch, declaring it as parent — or, where the
  unit's work builds on a sibling's unmerged branch, that branch (*Stack
  Rulings*, below) — so review judges
  each unit's own diff (its red obligation tests flipping green plus the
  implementation), never the inherited structural demolition beneath it. A
  unit PR based on main would carry the whole demolition diff and be
  unreviewable. The one exception is the obligation record's `Lane:`
  line, judged at conversion (SKILL.md, *Queues and Finds*): a converted
  queue item marked `main` cuts its branch from main, dispatches with
  `integrationBranch` set to main, and lands as an ordinary reviewed PR
  against main — riding the structural lane would couple its landing to
  the whole ledger draining. Any unit touching what the phase reshaped
  rides the lane like any other. A `Lane: main` obligation's evidence
  is exempt from the frozen-tip reachability check at promotion — its
  commits answer to main, not the lane — and its PR follows the ordinary
  WIP-sweep rules at run close like any other reviewed PR.
- **Red the ledger owns is not the unit's red.** A failing test cited by
  an open ledger obligation is expected state, not a defect for the unit
  PR to fix or be blocked on; only red the ledger does not account for
  counts against a unit. The review system is configured to honour this
  distinction; when reviewing a project where it is not, surface that as
  setup the user owes before the first landing, rather than letting units
  deadlock on inherited red.
- **Conditioning precedes the first unit.** The structural phase never
  runs the toolchain, so the handover tree may not build or run its suite.
  Before any unit dispatches, make the tree build and the suite execute —
  stubs and compile fixes, not capability work — as direct commits to the
  structural branch, exempt from the rule that every change serves a named
  obligation. This is the baseline step of reconciliation
  (references/ledger.md): its recorded result is the baseline artefact,
  and the point of it is that every later unit PR is buildable and
  judgeable on its own diff.
- **If main moves mid-run, absorb it into the structural branch** before
  the next unit merges, so conflicts with demolished code surface in the
  lane immediately rather than accumulating in the promotion diff.
- **Promotion is the run's one merge to main.** The structural phase
  opened a `WIP:` promotion PR (structural branch → main) at handover; it
  stands red while obligations remain, and its number is recorded in the
  ledger header. When the ledger is drained — every record `proven` or
  `waived`, the assembled-whole estate result in evidence — the branch
  goes to the QA campaign, and the campaign's merge-ready close is what
  de-WIPs and merges the promotion PR (SKILL.md, *Handing Off to the
  Campaign*). The autonomous review system never reviews or merges
  structural PRs, so that merge is a seat's own release act, gated on the
  drained ledger and the campaign's recommendation, and journalled by
  whichever seat performs it. Before
  taking the estate result, freeze the lane — no unit merges into the
  structural branch afterwards — and check reachability: every commit a
  `proven` obligation's evidence points at must be reachable from the
  frozen tip, or the tip does not contain the work the ledger claims —
  merge those unit branches in first (re-freezing on the joined tree) so
  the tree the estate proves is the tree promotion merges. If main has
  moved since it was last absorbed, absorb it into the lane before the
  estate result too — promotion must not combine the tested tip with a
  newer, untested main. Only unit PRs whose work no `proven` obligation
  claims may be retargeted to main after promotion lands, where they
  proceed as ordinary reviewed PRs.

## Stack Rulings

One invariant governs units that build on each other's unmerged work:
**dependency is declared by PR targeting.** A unit whose work builds on
another unit's unmerged branch opens its PR targeting that parent
branch — dispatch the child with `integrationBranch` set to the parent's
branch, and the pipeline's merge-base, sibling-duplication, and PR-base
mechanics all follow — so the PR graph is the dependency declaration,
review judges each PR on its own commits, and chains land bottom-up.
Bottom-up landing needs no manual retarget and no restack: merging a
parent PR with a true merge commit and deleting its branch retargets its
child PRs onto the parent's base automatically, leaving them open and
mergeable, their diffs still scoped to their own commits. What content
belongs in which PR stays a judgement call. Beyond that, a stacked PR
follows the ordinary latch discipline; the rulings below answer the
moments of doubt.

- **A base with open dependent PRs is amended only through a validated
  path.** The amended base runs the project's own gate green at the
  sender before the push — a failing commit pushed to a base poisons
  every dependent PR's automated review round with inherited breakage.
- **A legitimately rewritten base** (rewritten, not merely landed) is
  followed by one mechanical cascade, owned by the landing seat:
  `git rebase --update-refs` (git ≥ 2.38) moves every branch pointer in
  the chain in one command; then re-run the reviews the movement staled.
- **Stale, not failed.** A review verdict is evidence about the exact
  base and head it examined — verdicts already record the tree sha they
  judged. When a base moves under an open PR, dependent verdicts are
  stale, not failed: the response is a re-run, never in-PR repair of
  breakage the PR inherited.

## Direct Fixes

Some changes are too small to earn a pipeline: routing a ten-line repair
through verdict rounds, a WIP PR, and the review-system merge queue spends
hours of ceremony guarding nothing a glance could not judge. A **direct
fix** is mechanical, small enough to judge whole at a glance — tens of
changed lines at most — and adds no behaviour of its own: it restores the
composed tree to what its landed units individually proved (a lint break,
a missing adaptation at a seam between individually-green units), or
makes an equally tiny cleanup with no observable effect (a dead import,
an identity conversion). A direct fix commits straight to the structural
branch — no unit, no verdict rounds, no PR — exactly as conditioning
does; its proof is the repaired gate (or the check the change touches)
run green on the lane tip with the fix applied. Record the commit sha and
the green run in the journal; where an open obligation already cites the
repaired gate (a lane-health record, say), add the same evidence there —
never create an obligation for a direct fix. The push follows
`push_policy` as ever.

The boundary is a judgement with anchors, not a count. Route the change
through the ordinary pipeline the moment any of these holds: it adds
behaviour of its own, it needs a design choice, it outgrows a glance
mid-work, or
you find yourself wanting a second opinion — wanting one is the signal
that this is a unit. Direct fixes land on the structural branch only:
main is the review system's to guard, so a `Lane: main` repair stays an
ordinary reviewed PR. A direct commit moves the lane tip like any merge —
a recorded preview proof against the old tip invalidates as usual — and
after the promotion freeze, a needed repair re-opens the freeze: apply
it, then re-take the estate result on the repaired tip before promotion
merges.

## The Fixes Batch

Small queue items deserve landing, not ceremony — and not a flood of tiny
PRs either: every PR the run opens is a review the autonomous system must
run, so a dozen ten-minute fixes opened individually cost that system more
than the fixes cost the run. Queue fixes too small to earn their own
units land together through a **fixes-batch obligation**: an ordinary
ledger record (slug `fixes-<open-date>`, or `fixes-<open-date>-main` for
a second batch on the other lane) whose Sources are the queue items it
collects, with its `Lane:` set like any conversion and one batch per
lane — a branch cannot be cut from and merge into two lanes. Being an
ordinary record is what wires the batch into everything already built:
its PR state lives in the record, the resume sweep and reconciliation
verify it, the promotion freeze counts its commits through its `proven`
evidence, and the `(→ <batch-slug>)` markers on its source items retire
at its terminal flip exactly as any conversion's do.

What qualifies is the gap between the direct-fix bar and a unit: the fix
adds or changes behaviour a glance cannot judge, but is small enough that
one focused check — the test it adds or repairs, the command it mends —
proves it, and it needs no design choice a brief would carry. A mechanical
repair below that bar stays a direct fix (above); work that needs a design
choice, or outgrows the batch mid-fix, converts to an ordinary unit. A fix
that would touch an in-flight unit's seam waits or converts — the batch is
never a side door into another unit's files. Admission holds per fix: the
batch brief names the spine exemplar covering each fix's files, and a fix
no exemplar covers does not join the batch — it routes `needs-spine` or
converts alone.

The batch builds as a unit of its obligation on the ordinary pipeline —
the Template Rule knows no batch exception; the batch economises PRs and
reviews, never verification — on a branch `fixes/<batch-slug>` cut from
its lane. Unrelated fixes share the branch, not a fate: each lands as its
own commit with its focused proof recorded in the record's evidence, so
reverting one takes one commit, not the batch. The pipeline's verify
stage reads the whole batch diff against the source items it claims to
fix — the per-fix proofs plus that composed read discharge the record —
and sub-severe residue files to the annexe issue log as ever.

The batch lands at a judgement-sized cadence: open its PR — targeting its
lane, ordinary latch discipline keyed on the record — once it holds a
coherent handful, or sooner when a fix is time-sensitive; never per item,
and never one end-of-run dump, which serialises close behind a single
review. A second handful later in the run is a second batch obligation,
its own slug and branch. Under direct-merge policy there is no PR: the
same branch merges once verified, same evidence. Source items are deleted
at the record's `proven` flip by the seat that flips it, the commit
message carrying the dispositions.

## Docs Units Land Direct

A documentation unit lands by direct merge under either merge policy —
into main under direct-merge, into the structural branch under PR mode
(main for a `Lane: main` docs obligation) — never as a PR. The autonomous
PR-review system reviews code PRs only, so a docs PR stands waiting on a
review that never comes; the docs pipeline's cross-family accuracy audit
and triage hold are the unit's review of record, so nothing unreviewed
rides the merge. The pipeline's landing stage performs the merge itself:
a conflicted merge follows *Landing Conflicts* below, and the push
follows `push_policy` as ever.
The merge moves the lane tip like any other — a recorded preview proof
against the old tip invalidates as usual — and after the promotion
freeze a docs merge re-opens the freeze exactly as a direct fix does.
A `Lane: main` *code* repair still lands as an ordinary reviewed PR
(Direct Fixes above); the review system's guard on main does not cover
docs.

## Landing Conflicts

A conflicted landing merge is the landing stage's to resolve, not a
round-trip to your seat: the stage resolves union-preserving — keep both
sides' behaviour, informed by the known sibling overlaps the briefs
name — and the resolution stands only with the project's full gate green
on the resolved tree. What comes back to you is a genuine semantic
contradiction — the same knob given different semantics, intent the sides
cannot both keep — or a resolution whose gate stays red; both report as
`blocked_on: "conflict"` naming what contradicts what.

## Gate Venues

A check run proves one of two different things; keep them distinct,
because they are priced differently:

- The obligation's **consumer proof** — the journey or command its
  record's Proof line names. It discharges the obligation, and it runs
  where the latch requires: the unit's own tree, or a merge preview when
  the rung needs work the branch lacks (below).
- The project's **gates** — regression assurance for the composed tree,
  shared by every unit; they prove no obligation on their own. Every
  project keeps two gate recipes behind its entrypoint: the
  **check gate** — the
  static rung: build or type-level check, format, lints, and the other
  minutes-cheap deterministic checks — and the **full gate**, the
  whole-tree suite with its heavy test builds. A project without that
  split gets one as conditioning work at open, exactly as an unwired
  entrypoint does.

The gates run at named venues, never per stage. The **builder's** venue
takes the check gate, plus the tests covering the unit's touched
surfaces, on the tree its build finishes on: attempts there are its
own — it fixes and re-runs until green (references/pipelines.md, the
build stage) — and the final run is the one record, its command, tree
sha, and result in the report. The **full gate** runs at the lane
gate, batched over landed units (below), and — under PR mode — at the
forge's CI on the de-WIPed PR. Every stage between those venues — the
verifier, fix rounds, the merge preview — runs the **scoped slice**:
the checks covering the unit's touched surfaces. Fix-round commits
therefore land after the recorded run, leaving the record's sha behind
the final head — expected, not stale; the fix delta is verified
scoped, and the lane gate covers the composed result. A stage
that distrusts the recorded run for a nameable reason (missing,
contradicted by what it examined, or claiming a tree the unit's branch
never held) surfaces that reason as a finding first; re-running the
gate to settle the finding is legitimate, a quiet re-run in the
record's place is not.

## The Lane Gate

The full gate runs over batches of landed units, never per unit: one
run on the integration lane's tip — the structural branch under PR
mode, main under direct-merge — covers every unit landed since the
last green lane gate at a fraction of the per-unit cost. Between lane
gates, landed units stand on their check-gate records and scoped
slices; a defect only the full gate catches surfaces at the next lane
gate instead of blocking each landing — the deliberate trade that buys
the run its build throughput.

- **Cadence is a judgement with anchors.** Run it when the units landed
  since the last green lane gate would make finding a culprit painful —
  a handful, sooner when they touch shared surfaces — and always
  before taking the estate result at the promotion freeze (*PR-Mode
  Topology*) and at run close under direct-merge. The reconciliation
  baseline (references/ledger.md) stands as the run's first lane-gate
  result.
- **Run it through the project's gate entrypoint** on the lane tip —
  yourself, or as a dispatched run-and-report worker: the deliverable
  is a result, not code, so a bare worker is sanctioned
  (references/pipelines.md, *The Template Rule*). Journal the command,
  the tip sha, and the result; an obligation citing lane health takes
  the same evidence.
- **A red lane gate is sorted, not obeyed.** Red the ledger already
  owns stays with its record; a failure the batch introduced routes to
  a fix unit — or a direct fix where it is mechanical (*Direct
  Fixes*) — and when the culprit is not evident, bisect the batch's
  merges on the lane with the gate command. The lane does not freeze
  for a red gate short of the promotion freeze, but no promotion, run
  close, or estate result rides a tip whose lane gate is red.

## Proving Against a Merge Preview

A unit's proof runs on the richest tree it needs, and never waits on a
merge to exist:

- **The unit's own tree** proves most units — with capability and its
  first consumer built as one unit (SKILL.md, *Sizing at Dispatch*), the
  consumer proof usually runs on the unit branch alone.
- **A merge preview** proves a unit whose rung needs work the unit branch
  lacks — sibling units, lane commits since the branch was cut. Merge the
  current lane tip with the unit head — or heads, for a sibling joint
  proof — in an ephemeral worktree, run the
  proof there, and record every input sha in the Evidence. The preview
  *is* the
  tree the merge will produce, so proving on it keeps the invariant that
  no landing rides a tree no verifier saw — it is the sibling joint
  proof's composed ref (references/briefs.md), generalised to any unit.
  The preview's own question is composition — does lane tip plus unit
  head build and hold together — and the scoped slice answers it (Gate
  Venues, above); the consumer proof runs on the preview only where its
  rung actually needs the preview's extra work.
  A preview is evidence only while the lane still stands at its recorded
  sha: de-WIP checks that (The Latch, below). A moved lane always means
  a fresh preview — the merge is cheap — but what re-runs on it follows
  the lane delta: when the delta is disjoint from the unit's touched
  surfaces, the scoped slice revalidates the recorded proof; when it
  touches them, or the unit's work is cross-cutting (the build system,
  shared interfaces, global configuration), the consumer proof re-runs.
  Record the fresh preview's input shas and what ran either way.
  After de-WIP, the merge itself is the autonomous review system's to
  judge on its own checks.
- **A proof that cannot run even on the preview** — it needs units that
  have not been built yet — is not this unit's proof: it is estate-level,
  and belongs on the `assembled-whole` record or its own estate
  obligation. Move that part of the Proof line to the estate record and
  leave the unit's Proof covering what its tree or preview can show — the
  obligations' Must-be-true fields are untouched; only where each proof
  runs changes, so nothing is weakened to fit the unit.
  A unit PR gated on unbuilt future work is mis-scoped proof, never a
  reason to hold the latch shut.

CI builds the merged result on its own schedule; the preview proof is this
run's evidence and does not wait for it.

## The Latch

- **The prefix is a two-way latch, and its removal is yours alone.** Take
  it off only when the unit's obligation is `proven` — on its own tree or
  a merge preview, per the section above — no finding is open
  against the unit (every severe finding explicitly dispositioned; every
  sub-severe finding dispositioned or accepted at the land and filed to
  the annexe issue log from the envelope's `issues_findings` — an
  accepted sub-severe left unfiled is still open),
  and no dependent unit can still force a change. Before the PATCH,
  confirm by API read that the PR head is the verified tree (or the unit
  head a recorded preview proof names), that a preview proof's recorded
  lane sha is still the lane tip — a moved lane invalidates the preview;
  re-preview, re-running per the lane delta (Proving Against a Merge
  Preview, above) — and that the body
  is the rewritten report-derived text — each can silently be otherwise.
  Put the prefix **back** before dispatching any work against a released
  branch.
- **PR state is ledger state.** Record each open PR's number and its unmet
  conditions in the obligation record; a resume sweeps these first,
  releasing any now-eligible PR rather than re-deriving its unit as
  pending.
- **No run closes with a PR silently at WIP**: de-WIP it, report it
  blocked naming what it awaits, or close it unmerged with a one-line
  reason where its unit was abandoned.
- **A de-WIPed PR belongs to the autonomous PR-review system** — never
  merged by you or a worker. The one exception is the promotion PR
  (*PR-Mode Topology*, above), which that system never touches and the
  QA campaign's close merges.
- **Merge order is wired, not narrated**: a stacked dependent's ordering
  is already carried by its PR target (*Stack Rulings*, above); an
  ordering targeting cannot express (an estate PR composing others'
  work) becomes forge PR dependencies, confirmed by reading the list
  back; clear the list before closing a blocked PR unmerged.
- **Housekeeping**: at the first PR of a run, switch on the repo's
  delete-branch-on-merge default if it is off. When reading PR state on
  resume, check git reachability alongside the forge record — a
  hand-merged or superseded PR reads `closed, unmerged` while its commits
  sit in main; report the disagreement, never redo the work.

## Run-Level Guards

Apply after every pipeline run:

- **Pins are verified, not assumed.** Read the run record's per-model
  usage for the models that actually served the run — a missing or renamed
  model silently rides the ambient default, and a verdict from the wrong
  family is not the cross-family property the design claims.
