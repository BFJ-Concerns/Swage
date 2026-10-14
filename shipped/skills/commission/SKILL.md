---
name: Commission
description: Run a deliberate, heavyweight commissioning conversation that builds joint understanding of what a project or feature should be, and record the settled intent as the project's commission. Use when the user wants to commission, plan, scope, or capture intent for a project or feature; change what an existing project should be; migrate or sharpen a prose contract into falsifier-bearing commitments; settle how the project is proven (the consumer-proof ladder); retire or revisit a recorded Decision; blind-re-derive a design from first principles; hold a fully planned feature from building, revise, release, or withdraw a held feature; triage IDEAS.md or ISSUES.md into the contract or task queue; re-triage the task queue; or resolve an orchestrator's contract gap. Not for implementing or orchestrating the build, nor planning how to build work already scoped, and not for changing the spec of something the project produces (a skill or library it ships) — ordinary work, not project intent.
---

# Commission

Planning is building joint understanding between the user and the model — not
extracting requirements and transcribing them. The conversation runs in both
directions, and four jobs define it:

1. **Establish the end result** — concretely enough that no downstream agent
   has to guess.
2. **Fill the user's knowledge gaps** — bring what you know; complete the
   user's picture rather than planning around the hole.
3. **Hunt the user's incorrect assumptions** — actively. A wrong assumption
   baked into the contract is the most expensive kind of error.
4. **Spot overcomplication** — a simpler route to the same end result is a
   finding; raise it.

The four jobs are yours — the agent in the conversation. None of them is
subcontracted: dispatched work exists for one purpose only, perspective
unpolluted by this session's context or this model's biases (see Misconception
Hunting). The jobs stay in the room; only perspective is outsourced — and the
strongest way to escape this model's biases is to ask a *different* model
family, which is what the cross-family workflows do.

The posture across all four is adversarial-collaborative. Hold your own view
of the problem alongside the user's: where you would have done it
differently, say so; where their picture has a gap, raise it unprompted.
Challenging the user's framing is the job, not overstepping. The costliest
failure this skill exists to prevent is the gap the user doesn't know they
have — a proposed approach that isn't the best available, and nobody in the
room positioned to notice. When you see a better route, show it.

All four jobs always apply — small asks are statistically where "there's a
library that does this whole thing" lives — but ceremony scales with the
intent delta. The structure lives in these jobs and the artifact, not in
stages or templates: judgement over scaffolding throughout.

## What Counts as a Commission Change

The commission is the project's *own* intent — its purpose, architecture, and
the feature set it offers. Changing the spec of something the project
*produces* — a skill it ships, a library it builds, one output among many — is
ordinary work, not a change to project intent, however significant that change
feels. Significance is not scope: redesigning a single produced thing can be a
large job and still sit below the contract.

Before treating a change as commission-level, check that the README actually
speaks to it — that it states a principle the change would overturn. A README
that merely *names* something the project produces (as an example, a
deliverable, a build target) does not make every decision about that thing's
internals a contract matter. The evidence is the README's text, not the size of
the change.

When the check is genuine but unsettled — the change really might touch stated
intent — settle it with the user directly, here in the conversation. A present
user resolving intent live *is* this mechanism; there is no separate ritual
owed before the call can be made. Changes that don't touch the contract are
settled directly too, or logged for later — never deferred to a commission pass
that isn't needed.

## The Hand-off

The commissioning conversation's product for the build phases is exactly
one artefact: the signed commission — `README.md` committed in the
annexe. The structural phase (the maestro workflow) starts only from
that file; the volume build (the assembly workflow) starts only from the
structural phase's `build/` state. Nothing else this conversation
produces — a scaffolded annexe, working files, an unsigned draft — is a
hand-off, and the downstream workflows refuse until a signed commission
exists. That refusal is correct gating, never a defect to work around.
It gates on the committed file alone: once a `README.md` has been
signed and committed, the build phases start from it even while a
later, unsigned revision sits in the working tree — they neither see
nor wait for the revision.

Three consequences, binding:

- **The ceremony is not skippable.** However small the intent, the
  handshake runs and the README lands — is written, committed, and
  pushed with the sign-off publication — only on the user's explicit
  sign-off reply (Sign-off, below). There is no fast path that writes a
  README without one, and nobody signs on the user's behalf: a session
  driving this workflow with no user answer available ends unsigned
  rather than inventing a yes.
- **End legibly.** A conversation that ends without the sign-off yes
  states its actual state in its closing message, and the state differs
  by whether a signed commission already stands. **Initial capture** —
  no committed `README.md` in the annexe: say what stands (a scaffold,
  an unsigned working-tree draft), that no commission exists yet, and
  that the structural and build workflows will refuse until a later
  conversation lands the signature. **Unsigned revision of a signed
  commission** — a committed `README.md` stands and only this
  conversation's change is unsigned: say that the committed commission
  stands and remains the contract the build phases read, that the
  proposed change is an unsigned working-tree diff they do not see,
  and that a later conversation resumes the diff for its sign-off
  read; the downstream workflows do not refuse in this case — they
  build against the last signed text, so the closing message must not
  claim otherwise. In both cases an unsigned draft in the working tree
  is a legitimate resume marker; presenting it as commissioned is the
  failure, and so is telling the user a standing commission has gone.
- **Decline what needs no commission.** An ask that is ordinary work
  rather than project intent — a fix, a change to something the project
  produces, a task an agent could just do (the boundary in What Counts
  as a Commission Change) — is declined at the door: say it needs no
  commission and route it as ordinary work. Do not scaffold an annexe
  for it.

## Start From a Current Clone

Before reading the README as the contract, bring the repos this conversation
relies on level with their remotes: the annexe, whose README is this skill's
memory, and the code repo, which the shedding sweep grounds its compression
against. A clone edited from another machine reads stale — you reason from an
out-of-date contract, shedding grounds against code that has since moved, and the
closing commit collides with remote work it never saw. The scaffolder already
clones an annexe present on the remote but missing locally (see The Annexe); this
gate covers the other case — a local clone that exists but has fallen behind.

For each such repo present locally with a tracking remote — a true greenfield with
nothing pushed yet has neither, so skip it — `git fetch`, then read the state:

- **Up to date** → proceed.
- **Behind, working tree clean** → `git pull --ff-only`, then proceed.
- **Behind, working tree dirty** → do not pull. The uncommitted tree is likely an
  unfinished conversation's draft — the resume marker the next session picks up
  (see Reading the Situation) — and a pull would fight it. Surface the state and
  let the user decide.
- **Diverged** (local and remote each hold commits the other lacks) → stop and
  surface it; the user reconciles before continuing.
- **Remote unreachable** → say currency could not be verified, and ask whether to
  proceed against the local clone or wait, never silently as though current.

The gate is bound to grounding, not just to the conversation's opening: when a
repo the opening never touched first becomes the basis of a what-exists or
is-this-already-built judgement mid-conversation, give it the same
fetch-and-read before trusting what it holds. A clone checked at the start can
be the only current one in the room — its freshly-verified sibling proves
nothing about the repo grounded against an hour later.

## Reading the Situation

No modes, no flags — read the situation from the world. Every situation here
takes *this* project's commission — the contract of the repo the conversation
was invoked in — as its subject. A request that mentions the commission skill,
the tooling, or another repo is nearly always giving motive or context, not
naming a different target: read it against this project first, and treat a
parse that aims anywhere else as a sign you have misread. If the request
genuinely cannot be about this project's contract, ask which target is meant
and do nothing — no investigation, no dispatch — until the user answers.

- **No annexe repo** → greenfield full capture. Before concluding greenfield,
  confirm none exists remotely either: the scaffolder (The Annexe, below)
  probes the project's forge and clones an annexe that exists there but isn't
  checked out locally — that case is an update conversation missing its clone,
  not a green field. Capture-time is when
  anchors set hardest — when the user arrives with a load-bearing *how* where a
  *what* would do, survey the option space before it becomes contract
  (Misconception Hunting, the divergent survey). And once the user's intent is
  extracted, greenfield is a natural moment to offer the blind re-derivation
  (Misconception Hunting) as a counterpoint to the whole design before it is
  signed.
- **Annexe exists, a feature or change is named** → update conversation. Open
  by reading the README — it is this skill's own memory — and `held/` beside
  it: a feature already held must be recognised, not re-planned into the
  contract. Check the
  working tree: uncommitted README changes are an unfinished conversation's
  draft, picked up rather than started over. Check the new intent
  against the standing contract — recorded Decisions, commitment lines, and
  their elaboration alike: elaboration binds as the reading of its line, so a
  contradiction with it is as real as one with a Decision, surfaced and
  renegotiated, never silently drifted past. Check it against `shed/decisions.md` too, where
  one exists: a delta that re-proposes a retired choice is surfaced with the
  retirement's reason for the user to reopen or re-decline — not silently
  re-adopted, and not blocked on the ledger's say-so alone. When the named change carries a *how* where a
  *what* would do — a tool, a language, a mechanism offered as the route to an
  outcome — treat the *how* as falsifiable and survey the option space before
  integrating it (Misconception Hunting, the divergent survey); the delta the
  user came with is exactly where the costliest unexamined assumption hides.
  When the delta is a major feature or reopens a settled area whole, offer the
  blind re-derivation as well (Misconception Hunting) — the survey walks one
  decision's options; the re-derivation checks the design's starting place.
  Then integrate the delta; and because you are changing the contract anyway,
  run the shedding sweep — what the build has since embodied is grounded
  against the code and compressed, then the preservation passes confirm nothing
  was lost ([references/shedding-workflow.md](references/shedding-workflow.md)) —
  and the tend sweep over `held/`: every landed contract change advances each
  held file's pin or surfaces the breakage to the user
  ([references/held-intent.md](references/held-intent.md)).
  And every update triages the standing queues alongside the named delta: read
  the build phases' deferred-gap queue — `build/gaps.md`, and a legacy
  `maestro/gaps.md` too when one exists; drain both — and resolve each
  deferred gap into the intent artefact it belongs to: the README for
  contract questions, `VERIFICATION.md` for a gap asking for the proof
  ladder (both through the ordinary handshake and sign-off), or a
  `TASKS.md` item for the rare gap that proves a small fix rather than
  intent. Then triage `ISSUES.md` in full. An entry is one agent's unverified
  claim, not an established fact — the reporting agent may have misread the
  code, and the code may have moved since — so read an entry against the
  current code before disposing of it, with scrutiny proportional to what
  acting on it would cost. Destination finds are candidates for
  the README, decided journey and structural work goes to `TASKS.md`
  kind-tagged as it lands, idea-shaped finds are
  surfaced to the user, and stale entries — including those the code
  contradicts — are struck. One more disposition: an entry that describes no
  defect, only a change that fought back, is moved to `FRICTION.md` as a dated
  entry, joining the recurrence evidence rather than the work queues. Triage ends with
  `ISSUES.md` empty: every entry is deleted on disposition, the removing
commit's message carrying it — no drain narrative, resolution, or correction
is written into the log itself. Annotating an
  entry in place, judging it minor, or labelling it for the orchestrator is not
  a disposition — a find too small for its own `TASKS.md` item rides an
  existing item as a rider or is struck with a one-line decision, and an entry
  stays in the log only when the user explicitly defers it. A long log is no
  exception — volume changes how the triage runs, never whether it runs: when
  serial reading would drag, fan the against-the-code reading out to
  dispatched workers and settle each returned disposition here. Proposing a
  separate triage sitting is the deferral this rule bars. The orchestrator
  already takes the unambiguous, clearly-journey finds during its runs, so what
  reaches you is the remainder that needed a scope or intent call — exactly the
  finds triage exists for. A resolved gap or destination find is a *candidate* for the
  contract, not a write to it — it settles through the same handshake and
  sign-off as any intent, so neither queue becomes a side door into the README.
  This review is a fixed beat of every update, the same standing as the shedding
  sweep — never optional — and each resolved gap or find is deleted from its
  queue: the ruling lives in the artefact it landed in and the removing
  commit's message, never as an annotation left in the queue.
  `TASKS.md` is re-triaged in the same beat. Tag every untagged item
  `(journey)` if an assembly run can absorb it or `(structural)` if it
  needs the structural phase; the build seats honour the tag rather than
  re-deriving an item's kind from prose.
  Re-judge every item — open and blocked alike — against the current
  code and contract: still wanted, keep it; changed in
  kind, re-tag it; superseded or no longer wanted, delete it with the
  user's agreement, the commit message carrying the disposition;
  ownerless, surface it to the user; blocked, re-check the named
  blocker — cleared means the marker comes off and the item is open
  work again. A backlog that only ever grows is itself a finding:
  surface it to the user with the oldest items — by their `(added
  <date>)` stamps — named, rather than
  re-certifying it silently.
  Corrections rewrite an item's own
  line — nothing is appended beneath it — and items are the only queue
  state a build run reads: a prose section claims nothing. Close by
  setting the file's `Last triage:` header line to today — the stamp
  certifies that every item was actually re-judged, so it is written
  only afterwards. `FRICTION.md` is read in the same beat but works the opposite
  way: look only for recurrence — the same area appearing across entries — and
  corroborate before proposing, against the repo's own history
  ([references/friction-signals.md](references/friction-signals.md) names the
  patterns worth trusting). Ledger recurrence plus a structural pattern in the
  history plus a worsening trend is a case; a single entry, a lone feeling, or
  an area that is unpleasant but stable is not. A case earns a
  scoped refactor proposal to the user: what hurts, the evidence, the refactor,
  and which stated wants it could break. The refactor is spun off as its own
  task — folding it into the update that surfaced it widens that update past
  its brief. Entries persist as the recurrence evidence rather than being
  drained, with two prunings owned by this triage: an entry whose case is
  promoted into decided work is deleted with the promotion, and an entry
  whose area no longer exists in the code is deleted as stale. An
  assessment's conclusion is recorded beneath the entries
  it judged, including a checked-and-cleared one, so the same lead is not
  re-investigated next quarter. Entries already covered by a recorded
  conclusion are skipped unless newer entries have joined them since.
  Every update also brings the annexe's managed scaffold files back into step
  with current conventions — the orientation `AGENTS.md` and its `CLAUDE.md`
  import, `config.toml`, the `TASKS.md` legend header, and the code repo's
  `CLAUDE.local.md` pointer. The
  scaffolder writes each only when absent (see The Annexe), so a file whose
  template has since moved on is never refreshed on its own. Re-run the
  scaffolder to restore anything missing, then read the existing managed files
  against current conventions and bring a drifted one current — correcting a
  file that merely fell behind, and surfacing one that was deliberately
  customised for the user to decide rather than overwriting it. A managed file
  that has grown an executable procedure where a setting would do is cut back to
  it (see The Annexe), not refreshed in place.
- **A feature is named to settle but not build** ("nail this down, but don't
  work on it yet") → hold conversation: the full capture machinery — handshake,
  elicitation, hunting moves on their signals — with the settled text landing
  in `held/<slug>.md`, never the README. The user releasing a held feature
  ("we're ready to build X now") is a narrow move of its own — unconditional
  revalidation, then one commit that splices the held file into the README
  and deletes it — as are revising a held feature in place and withdrawing
  one. All the moves, the trust model, and the tend sweep are in
  [references/held-intent.md](references/held-intent.md) — read it whenever a
  hold, a release, or a non-empty `held/` is in play.
- **The user wants to go through IDEAS** → ingest processing. Each item is
  promoted into the What, rejected (delete it; record a Decision only if the
  threshold is met), held (the user wants it settled now but not built — a
  hold conversation, above), or left parked; an investigation item ("look
  into X") is instead run from the conversation or converted into a `TASKS.md`
  item, whichever the user prefers. Delete every processed item, so only
  parked items remain.
- **An orchestrator routes in a contract gap mid-run** → a one-line gap is a
  one-minute exchange: ask the user, handshake the answer, write it, get the
  proportionate sign-off. Small, but never a side door.
## The Annexe

Each code repo `foo` pairs with a working repo `foo-Annexe` on the same forge
account as the code repo, checked out as a sibling directory. Root = contract; the build
subdirectory (`build/`, or a legacy `maestro/`) = scratch.

| File | Role |
|------|------|
| `README.md` | The commission — this skill's artifact. The forge renders it on the repo front surface; agents read READMEs by instinct. |
| `IDEAS.md` | The user's zero-ceremony inbox: anything they want to bring to a commission pass — feature ideas, open questions, things to look into with an agent later. Written by the user alone; processed only by a commission conversation. Agents route idea-shaped findings through their own reports for the user to triage in. |
| `ISSUES.md` | The shared find-ingest log: bugs and tangential finds written down in passing, appended by agents and user alike, with no judgement of scope or fix — each entry a reporter's unverified claim until triage checks it against the code. This skill is its triage owner and works the whole log — destination finds into the README, decided work into `TASKS.md` kind-tagged, idea-shaped finds surfaced to the user, friction-shaped entries (no defect, just a change that fought back) moved to `FRICTION.md`, stale entries struck. The orchestrator takes the unambiguous, clearly-journey finds itself between commission passes to keep them moving — its habit, not a disposition a triage may choose; every find that needs a scope or intent call waits for this triage, and a commission pass never leaves a find behind for the orchestrator. A disposed entry is deleted from the log, the commit message carrying its disposition. |
| `FRICTION.md` | The friction ledger: dated observations of a codebase area resisting change — area, symptom, expected-versus-actual — appended by agents whenever a change fought back harder than the work warranted. Reviewed by commission updates for recurrence, which is assessed against the repo's history and answered with a scoped refactor proposal to the user; entries persist as the recurrence evidence — pruned only when a commission pass promotes a case into decided work (deleted with the promotion) or finds an entry's area gone (deleted as stale) — with assessment conclusions recorded beneath the entries they judged. |
| `TASKS.md` | The decided-journey queue: already-triaged work that belongs to the build but not the contract, written by the user, the build phases, and this skill, and drained by the build phases by kind — `(journey)` items by assembly runs, `(structural)` items by maestro at its phase boundaries, each tag assigned by this skill's triage (untagged means awaiting it). A find downgraded from `ISSUES.md`, or a gap that proves a small fix, lands here action-phrased — never in the README. Items are single checkbox lines progressed by trailing markers, dates `YYYY-MM-DD` — `(added <date>)` written once when the item enters the queue and never restamped (the age that "oldest" resolves to; a stamp missing from a user-written item is added at the next triage or run that reads it), `(blocked on <blocker> since <date>)` when a named dependency (an obligation, a user decision, an external fact) stops the item, `(→ <slug>)` once converted into the build ledger — a run's fixes-batch obligation included. There is no capacity marker: an item a run cannot take yet simply stays open, and runs name the open remainder to the user at their boundaries. The header's `Last triage:` line records the pass that last re-judged every item. A converted item is deleted at its obligation's `proven` flip by the seat that flips it; a completed item whose work landed without conversion is struck by whoever lands it — either way the commit message carries the disposition, so the queue holds only work still owed. |
| `held/` | Held intent: features planned to full contract rigour but withheld from building — one file per feature, single-writer like the README, entered, revised, tended, released, or withdrawn only through this skill ([references/held-intent.md](references/held-intent.md)). Each file opens with a not-authorised banner and pins the README text (its git blob hash) it was last checked against. Build intent comes from the README alone; a held file reaches the build only by release — a commission conversation that splices it into the README. |
| `shed/` | Append-only archive of detail shed from the README — one dated file per shed feature (`YYYY-MM-DD-<feature>.md`), written only by this skill, never edited or deleted after. Each opens with a mandatory header: the shed date, and that it is historical record, not current intent — the README wins. Live Decisions stay in the README; retired ones move to `shed/decisions.md` (see the retirement rule). |
| `AGENTS.md` (committed) | Orientation for cold-landing agents: what the repo is, contract versus scratch, the commit discipline. It is the cross-tool source of truth that AGENTS.md-reading tools read directly; a sibling `CLAUDE.md` imports it with `@AGENTS.md` for tools that read that file instead. |
| `config.toml` | Runtime knobs — `merge_policy` and `push_policy` first, more as needed. Plain config, editable without a commission conversation; read by the orchestrator and any agent or script. Not the contract, not single-writer. |
| `VERIFICATION.md` | The consumer-proof ladder: what "prove it at the real consumer" concretely means for this project — one rung per kind of thing it ships (a CLI's binary invoked, a service hit with a live request, a UI driven, a library linked from a separate caller), with what to do when a rung's consumer is unreachable. It does not compete with the README's contract claim: the What's commitment register already defines what the thing is, and each line's falsifier is the acceptance shape — the ladder only operationalises how each commitment is exercised at its named consumer's boundary, and where a rung would add a new claim about what the project must be, that claim belongs in the What first. Single-writer like the README, and settled through the same machinery: rungs pass the handshake, the file rides the working-tree draft, and the sign-off read covers its diff alongside the README's. Settle it during capture for any commission that will be built — the elicitation probe is "how would you check this actually works, per kind of thing?" — keeping rungs journey-shaped, not tool-prescriptive. Probe the proof shapes a two-rung template misses, and settle each while the ladder is first drafted: behaviour that by construction exits before the product's surface is reachable (what discharges a live rung when the thing under proof is a refusal to start), in-process engine journeys (whether they stand as proof in their own right or as supporting instruments below the ladder), and invariants proven at storage level against promises proven at the consumer (which level governs which change) — each of these otherwise resurfaces as a build-run gap, one parked obligation at a time. Settle the dark-capability question alongside the rungs: whether a capability may ever land before a consumer. The default is no — capability and its thinnest real consumer build and prove as one unit (the build skills enforce this) — so a pre-consumer proof rung exists only where this commission explicitly allows one. The build skills read it for every proof and never write it; a build session missing it files a gap and parks the affected proofs. A rung reading settled after capture — a drained gap, a user ruling from a build run — lands in the ladder file itself through the ordinary handshake, never only in the gaps entry that raised it: briefs are written from the ladder, so a reading left outside it is re-litigated by the next brief. |
| `commission/` | This skill's own persisted records: the sign-off reports, the commitment audit's verdicts and exclusion diffs, and any other dispatched-work product worth keeping past the session — one dated subdirectory per round (`commission/<YYYY-MM-DD>-<round>/`). Never under `build/`: that directory is the build phases' state, and the structural phase reads its presence as resume state. |
| Build subdirectory (`build/`) | The build phases' shared state — spine inventory, obligation ledger, teardown records, unit folders — not this skill's business, save `gaps.md`, the deferred-gap queue every update conversation drains (see Reading the Situation). Older annexes may carry the predecessor's `maestro/` subdirectory as well as or instead of `build/`; every `gaps.md` present is drained. |

**Managed files hold the setting, never the procedure that executes it.** A
scaffold file carries the knob, its values, and a brief gloss of what each
selects (`merge_policy = "pr"` — workers open PRs rather than merging): the
knob's own interface, which stays stable. It does not carry *how* the chosen
policy is carried out — the commands, transport, or steps that execute it drift
when the real procedure changes, so they live in the skill that acts on the
knob. How a PR is actually opened is the orchestrator's to state, not
`config.toml`'s to recite; a `config.toml` that spelled out the exact PR-push
command is the drift this prevents, the command moving upstream while the copy
silently does not. A managed file that has grown such a procedure is cut back to
the knob and its gloss, not refreshed.

The code repo gets an uncommitted local pointer file at its root naming the
sibling annexe — a discovery pointer only: intent lives there. The `*.local`
gitignore convention keeps it unstaged. The working rules
(commit-as-you-change, README single-writer, IDEAS.md capture) are carried by
this skill and the annexe's own `AGENTS.md`, so the pointer stays a single
line and code-repo sessions never carry a second set of commit rules. A
greenfield project may have no code repo yet — the code repo and its pointer are
build-time business.

**Creating the annexe** (when missing): the annexe's name derives from the
code repo's — `foo-Annexe` — so the project name must be settled before the
annexe can exist on disk. Greenfield, the name is itself intent and naming it
is part of this job: propose candidates and settle the choice through the
handshake like any other item. Until then the draft lives only in the
conversation.

Once the name is settled, run the bundled scaffolder rather than hand-typing
the ritual:

```bash
scripts/create-annexe.sh --annexe-dir ../foo-Annexe [--remote github:<owner> | <host-alias>:<account>]
```

`--remote` is optional. A project whose code repo has a forge remote passes
the same account in the forge's own form — `github:<owner>` for a GitHub
account or organisation, `<host-alias>:<account>` for a Forgejo instance
reached through that SSH host alias; a local project with no remote omits it, and the
scaffolder then creates a local-only annexe — no probe, no clone, no push —
which gains a remote later with an ordinary `git remote add` and push.
Never hand-build a substitute annexe because the project has no remote.

**Prove the location before capture begins.** The annexe is a sibling of
the project, outside the attached workspace, and the sandbox may refuse
writes there: creating `../foo-Annexe` can fail with a read-only
filesystem while writes inside the project succeed. So the first thing
this conversation does on a project with no annexe — before the first
intent question — is run the scaffolder (or, when the name is still
open, probe the parent directory with a shell `mkdir` of the intended
path and remove it). If the write is refused, stop there and name the
single setup action: the operator attaches the project's parent
directory, or creates the empty sibling and attaches it, trusted and
read-write; then re-run the scaffolder and continue. One question, asked
once, before capture — intent capture never stops midway on a directory
the procedure could have proven writable at the start.

(The script lives in this skill's directory; run it from there through the
shell tool.) It is idempotent — it creates only what is missing, so the same
command is the
existence check, the first-time scaffold, and the repair for a half-built
annexe. It probes the forge and clones an annexe that already exists
there rather than clobbering it (the update-missing-its-clone case); on a true
green field it initialises the sibling repo, lays down the orientation
`AGENTS.md` (with a sibling `CLAUDE.md` that imports it via `@AGENTS.md`, for
tools that read that file) plus a `config.toml` stub and
empty `IDEAS.md`, `ISSUES.md`, `TASKS.md`, `FRICTION.md`, `held/`, and
`shed/`, writes the code repo's uncommitted
`ANNEXE.local.md` pointer when a code repo exists, commits, and — when a
remote was given — creates the private remote repo and pushes: on GitHub
through `gh repo create --private`, on a Forgejo instance through its
push-to-create, so the first push makes the repo.
Pass `--merge-policy` and `--push-policy` once they are settled, or write
`merge_policy` / `push_policy` into `config.toml` when the conversation lands
them.

Pick `--remote` as the same forge account the code repo uses — read it from
`git remote get-url origin`: a github.com remote gives `github:<owner>`, any
other host gives `<host-alias>:<account>`. Greenfield with no code repo: ask the operator which
account the project belongs to, or whether the annexe stays local for now.

**The repo exists from setup; the commission lands signed.** That first push
bootstraps the remote repo with its orientation and working files but no
`README.md` — the commission itself is never pushed unsigned. The README is
drafted locally (the working tree is the draft) and reaches the remote only on
the sign-off push that publishes it, so a scaffolded annexe with no README on
the remote is one whose commission is still unsigned.

**Single-writer rule.** The README has exactly one writer: this skill. Every
write, however small, passes the handshake and a proportionate sign-off. The
user's other input — stray thoughts, not-yet-talked-through wants, things to
look into later — feeds `IDEAS.md`; agent findings (an orchestrator's, a worker's) ride their own
reports for the user to triage, because `IDEAS.md` is the user's pen alone.
The rationale is absolute: every guarantee the file offers (items verified before
written, corrections cascade, sign-off covers every change) holds only with no
side door. One unverified write makes the file's trust status unknowable.

**The working tree is the draft; the commit is publication.** Items settle
through the handshake and land in the README as uncommitted edits; sign-off
reads that draft (the file itself on first capture, the working-tree diff on
updates); the closing commit — made on the sign-off yes — publishes the
change with the approval recorded in its message. That yes is the only ask:
it covers the commit and the push, so never ask separately for permission to
commit approved work. **Commit per meaningful change:** each approved intent
change is its own commit before the session ends, never batched with
another session's. An interrupted conversation may leave an unsigned draft
in the working tree — not ideal, but better than losing the work; the dirt
is the resume marker the next session picks up. Git history is the living-document mechanism — evolution,
audit, archaeology, all free. Plain descriptive messages stating the intent
change; no conventional-commit ceremony. Non-contract files (`IDEAS.md`,
orchestrator scratch) carry none of this — commit them as they change.

## The Commission (the Artifact)

The annexe's `README.md`, written by agents for agents: information-dense, no
conversational filler. Its sole purpose is to bring a fresh session to the
understanding this conversation reached.

**Destination, never journey.** The file describes what the finished thing is,
as currently agreed. No progress state, no done-markers, no increment log —
what is built versus pending is the orchestrator's problem. Intent moves only
when the user changes their mind, which is exactly when this skill is in the
room — so the file is structurally incapable of going stale from work being
done. Shedding grounds its candidates against build state to decide what is safe
to compress; that read gates an edit and never enters the file, so the contract
still records destination, not progress.

**Shape — wide headings, not a template:**

| Section | Status | Carries |
|---------|--------|---------|
| **Why** | always | Problem, purpose, who for. The generalisation engine: lets an agent derive the answer the user would have given. |
| **What** | always | The destination: the commitment register — numbered, falsifier-bearing commitment lines with their elaborations — plus the reader-protocol block ([references/commitment-register.md](references/commitment-register.md)). Journey-flavoured, orderless. |
| **Constraints** | always | External facts that shape the build: integrations, environments, hard limits. Nothing downstream can invent these. |
| **Glossary** | as earned | Last resort after standardising vocabulary — hunt for the world's standard term first; bespoke or ambiguous terms only. Emptiness is a health signal. |
| **Decisions** | as earned | Intent-level only, strict threshold: multiple viable options, costly to reverse, a future reader would ask "why not X?". Each tagged **examined** (alternatives were actually weighed — a survey ran, or the user compared options) or **provisional** (a choice named once but never compared). Job: stop settled questions being reopened — including by this skill's own future sessions; the examined/provisional tag is what stops that protection shielding an unexamined anchor (the reconciliation beat in references/misconception-moves.md). |

**A Decision leaves the contract only by explicit retirement.** Deleting or
materially weakening a Decisions entry — including as a side-effect of a
minimisation cull or a full rewrite — is itself an intent change: name the
retirement (or the superseding Decision) to the user as its own item, settle
it through the handshake, record it in the sign-off read, then append the
Decision's text, tag, reason, and date to `shed/decisions.md` — a single
append-only ledger in the annexe. A silently dropped Decision is invisible to
every later conversation, which will re-settle the question the other way.
The ledger works forwards too: when integrating new intent, check it — a
delta re-proposing a retired choice is surfaced with the retirement's reason
for the user to reopen or re-decline, never silently re-adopted or blocked.
Retirement also has a blast radius beyond the entry itself: other contract
text may lean on the retired mechanism — a rationale citing a cap that no
longer exists, a prohibition whose safety net the cap was. Before the
sign-off read, sweep the contract for lines whose wording or justification
rests on what was just removed, and re-settle each through the handshake —
it stands on a new footing the user confirms, or it retires too.

**Caps, budgets, and ceilings each settle as their own Decision.** Every
numeric or structural limit — an agent cap, a byte ceiling, a rate budget —
is put to the user individually and recorded with its dimension, default, and
rationale. A bundled approval ("I agree with your proposals") never settles
one implicitly — an unnamed limit inside a broad yes is how a choice enters
the contract without ever being chosen.

**The merge policy** is settled in this conversation but stored outside the
contract, in the annexe's `config.toml` — it governs how a finished unit of
work lands, a process knob rather than a fact about the destination. It is
still a choice the conversation must actively put to the user, because the
orchestrator reads it and may not invent it. Two modes: **direct-merge** —
workers merge to main themselves; propose for young projects where main is
cheap to fix — and **PR mode** — workers push branches and open PRs that wait
for the user's review system; propose for functioning projects where main is
load-bearing. Settle it through the handshake and write it as `merge_policy`
in `config.toml`; landing mechanics (branch pushes, forge-API PR creation,
stacking)
are orchestrator business, not this skill's. Because it lives in plain config
rather than the single-writer README, it can be changed later without a
commission conversation. A run that finds it unset settles it then.

**Writing rules:**

- **Every claim verifiable.** "The user does X and sees Y" passes; "good
  search experience" fails. If it can't be checked it isn't concrete enough.
  This is why there is no separate acceptance section: a verifiable What *is*
  the acceptance shape — a journey is acceptance written as narrative. Each
  committed capability is a commitment line — stable ID, named consumer,
  observable act, falsifier written at capture — which keeps the claim
  checkable where "the user sees Y" has no grip: internal and agent-facing
  capability. Read
  [references/commitment-register.md](references/commitment-register.md)
  before writing or rewriting the What.
- **No heading carries scope, and dependency is register material.** Sequence,
  phase, and distance from the present are attributes on a commitment line,
  never sections — a "later" heading is invisible to scoping readers who
  source work item by item. A commitment resting on unbuilt substrate names
  it in `stands on`, and signature waits until every entry resolves (the
  stands-on gate, same reference).
- **Milestones are goal lines.** An aim the user voices the work running
  toward — "dogfooding", "MVP", a launch — is captured as a goal line the
  moment it is voiced: the register's `G<n>` form, whose act is the
  end-to-end journey the milestone means and whose falsifier is written at
  the live product's boundary (the goal-line rules, same reference). From
  then on the milestone's name resolves to its line — a readiness question
  ("does anything block dogfooding?") is answered against that falsifier
  and the line's `stands on` set, never against a heading or a reader's
  sense of the word.
- **Meaning, not wording.** The user's phrasing is raw material, not contract
  text: write the verified meaning in the clearest words available. The
  handshake makes this safe — intent is transformed while its issuer is
  present to falsify the transformation. Quoted evidence is the one
  exception: it keeps its source's exact words.
- **Inferences are owned, not attributed.** A rationale the user actually
  stated is recorded as theirs; a rationale you inferred is your reading until
  the user confirms it — never written as the user's stated reason. An
  unconfirmed inferred rationale carries that mark in the draft and may not
  survive into a signed contract unmarked. How to assert an inference in
  conversation without laundering it is the handshake's elicitation half
  ([references/alignment-handshake.md](references/alignment-handshake.md)).
- **The What is orderless** and includes everything wanted-at-some-point that
  the user has no objection to seeing built now. Ordering and timing are
  runtime business. A feature the user wants settled but not built yet is
  held, not written: planned to the same rigour, landed in `held/`
  ([references/held-intent.md](references/held-intent.md)). Undecided or
  objectionable-now items stay out — they go to `IDEAS.md`.
- **Conviction is probed, not assumed.** The handshake verifies
  understanding, not wanting: a user can accurately confirm a reading of
  something they do not care about, which is how a model-proposed default
  lands in the contract dressed as intent. Before writing an item you
  proposed and the user merely accepted, ask which it is — a requirement, a
  preferred default, or a don't-care. Requirements are written; a
  load-bearing default is recorded as a provisional Decision, never smuggled
  into the What; a don't-care is deliberately left unwritten — silence is
  what grants the builder the freedom the user just declined to constrain.
- **Collision is checked at the pen.** Before a settled item lands in the
  README, read it against the standing contract — the What and the Decisions
  both, not the Decisions alone. Two rules can each be individually agreed and
  still pull against each other (one says repair, the other says terminate),
  and elaboration binds as the reading of its commitment line, so collisions
  hide in prose as well as in lines and Decisions. A collision is
  surfaced and renegotiated with the user before either side is written — the
  contract never carries both sides of a known contradiction. This read costs
  one deliberate look per write; the dispatched preservation review covers the
  rewrite case, where items land in bulk and no per-item moment exists.
- **Boundary statements inline, governed by the surprise test.** No non-goals
  section. A negative earns its place exactly where a capable agent
  generalising from the Why would plausibly build the wrong thing, each with
  its why — and a *binding* negative takes the register's numbered form with a
  `violated when <observation>` falsifier, placed beside the commitments it
  guards; prose around it explains, the line binds. And when the negative bans
  deciding something from a signal
  ("never decide X from Y"), the why is not enough: state what decides X
  instead. A bare ban invites the literal reading — no signal at all — and if
  no replacement judgement can be stated yet, the ban is not settled enough
  to write.
- **Qualities only where distinctive.** Elicitation probe: "where would a
  competent builder's default be wrong?" — answers go inline in the What, as
  elaboration under the line they qualify, or as the line's own falsifier
  where the quality is the commitment.
  "Snappy" and "good UX" are defaults, not information.
- **The surprise test runs in reverse too.** For a line being elaborated,
  ask: could a competent builder satisfy it in a way the user would hate? If
  not, the line is over-specified — generalise it until just before the
  answer flips. This is the removal pressure during capture; shedding handles
  the built, but a constraint that never earns its place is cheaper never
  written.
- **Shedding.** Once code embodies a behaviour, compress it to a sentence —
  the code is its own documentation. A commitment line is exempt: already the
  compressed form, it survives shedding verbatim — ID, consumer, act,
  falsifier — and only the elaboration beneath it compresses; a line leaves
  the register by descoping alone. The file is biggest when the project is
  youngest; the pressures cancel — but only if shedding actually fires, so it
  is a standard beat of every update, not an opportunistic afterthought. Shed
  detail is not destroyed: it moves to `shed/`, one dated file per shed feature,
  recoverable when a wrong implementation needs the full spec it was built
  against. "Once code embodies it" is a claim about the world, not an assumption
  to take on trust, so a shedding run grounds its candidates against the code
  before compressing, then the preservation passes confirm the rewrite lost
  nothing. The run is authorised by this skill's invocation; its beats and
  gates are in [references/shedding-workflow.md](references/shedding-workflow.md).
- **Elaboration proportional to conviction.** Committed items are register
  lines, fully elaborated; the aspirational tail stays out of the register at
  one non-binding line each. Conviction is whether the user wants it, not
  whether it is buildable yet: a committed item that depends on unbuilt
  substrate is still fully elaborated now, its dependency named in `stands on`
  so every scoping read sees it — ordering stays runtime business, the
  dependency itself never is. There is no "scope later": the
  commission conversation *is* the scoping interview, so source material
  promising a future scoping pass is an instruction to hold that pass here.
  Full elaboration means intent-level journeys, not implementation design.
- Absolute dates only. One term per concept throughout. Brevity targets
  roughly 1–3 dense surfaces — over-capture is over-specification.

**One live contract file per commissionable project, never sharded.** Exactly
one file — the README — claims currency about what may be built, so an agent
asking "what should this be?" reads one place and drift between rival accounts
is structurally impossible. If a destination can't fit one file, split the
*project*, not the document: multi-file staleness is silent and compounding;
single-file bulk is visible, linear, and benign at model scale. The other
annexe files don't shard the contract because none competes with its claim:
`shed/` claims staleness rather than currency, `held/` claims
verified-but-not-authorised (pinned to the commit it was checked against),
and `TASKS.md` and `IDEAS.md` sit below the contract entirely.




## The Alignment Handshake

The core mechanism: verifying that a receiver's understanding matches an
issuer's intent. The full protocol — six invariants plus per-level
application — lives in [references/alignment-handshake.md](references/alignment-handshake.md);
it is written to be borrowed standalone (the orchestrator applies the same
protocol to worker restatements).

The operating bar in this skill: **anything written into the README as
settled has been verified through the handshake first; trivia below that bar
is not.** Restatement is not a verbal tic after every exchange. And when a
correction lands, it invalidates the verified status of everything settled on
the old understanding — sweep the dependents.

## Elicitation Moves

The handshake verifies what has been said; the moves in
[references/elicitation-moves.md](references/elicitation-moves.md) surface
what has not — user-narrated journey walking, concrete-instance forcing, the
disappointment and success probes, and the why-ladder with its stop rule.
Read it when establishing or substantially extending the What: capture is
where the wants a user cannot volunteer as requirements are cheapest to
surface. Like the hunting moves below, they fire on their signals — never as
a questionnaire.

## Misconception Hunting

An always-on **posture** plus discrete **moves** fired on signals — never an
every-capture gauntlet.

**Posture:** the user's claims are falsifiable, not requirements.
Disagreement is licensed explicitly. Symmetrically, your knowledge has a
cutoff — verification settles arguments, not rank.

**Signals:** an approach where an outcome would do (anchor risk) — including a
tool or language offered as an example ("so like X"), which reads as a settled
choice but is anchor risk; a load-bearing factual claim (effort scales with how
much rests on it); vintage markers ("last time I checked"); complexity smell
(proposal harder than the outcome warrants); an important part — core to the
app, or infrastructure the build genuinely hinges on — whose option space nobody
has walked; your own surprise — if you would have done it differently, that
divergence is the signal.

Detecting an anchor is a cheap reflex, kept separate from the expensive
response: the moment a named tool, technology, or approach is offered where only
an outcome was required, note it — that costs nothing and does not depend on you
noticing you are already anchored. Then scale the response to stakes: a routine,
reversible choice needs no dispatch; a load-bearing or costly-to-reverse one
earns the survey.

The moves themselves — verify-don't-transcribe, the divergent survey, the
blind re-derivation, the draft attack — with their information diets, the
reconciliation beat that triages returned findings against examined and
provisional Decisions, the relay discipline, and the hard requirement that
triggered moves run as real cross-family workflows (stop and decide with the
user when a required model family is unreachable in the configured
catalogue — never a hand-prompted same-model substitute) are in
[references/misconception-moves.md](references/misconception-moves.md).
Read it when a signal fires, before dispatching the move, and again when
findings return.

## Sign-off

- **The object is the artifact, never a paraphrase.** Initial capture: the
  user reads the README itself — brevity makes this honest. Updates: the user
  reads the *diff* — working tree against last commit, exactly this session's
  change — proportional however large the file grows. The merge policy in
  `config.toml` is settled through the same handshake but sits outside this
  signed-off object — plain config, not contract, like `IDEAS.md` and
  orchestrator scratch.
- **The read is delivered as a sign-off read in the conversation.** Every
  sign-off presents the object — the file on first capture, the diff on
  updates — together with its decision cards, then waits for the verdict
  before anything is committed. Read
  [references/signoff-page.md](references/signoff-page.md) at every sign-off —
  it carries the read's structure, the decision cards, and the verdict
  handling. The user's explicit reply is the verdict.
- **The commitment audit clears before the read** on any round that worked
  product intent — capture, a feature delta, a hold or release, elaboration
  edits — whether or not a commitment line changed: the exclusion diff exists
  to catch wants voiced but never written, which is precisely a round that
  changed no line. Only a round that discussed no product intent at all (a
  pure queue triage, a config knob) skips it. Run the carried
  `commitment-audit` workflow
  ([references/ensemble-workflows.md](references/ensemble-workflows.md)) late,
  once the register has settled, over this conversation's transcript (located
  per Session Transcripts, below) and the whole draft register, with the
  hollow stage scoped to lines created or changed this round plus any line
  never yet audited. Its products are all acted on before publishing: each
  **hollow verdict** — a commitment whose cheapest compliant implementation
  defeats it — is closed by one of three dispositions: **sharpened** through
  the handshake; recorded as **covered by siblings**, where the complaint is
  one the register's other lines already answer (the verdict's
  `covered_by_siblings` field, or your own whole-register read, names them —
  record the naming lines in the sign-off report); or accepted as a
  **residual**, where sharpening would demand over-specification the surprise
  test forbids — carried onto the sign-off read as a decision card with its
  reason, and closed by the signature over it. The **exclusion diff** —
  wants voiced in conversation that match no commitment — is settled with the
  user (a new line, a hold, a non-binding tail mention or `IDEAS.md` entry, or
  a deliberate exclusion), and the deliberate exclusions ride the sign-off
  surface as decision cards, so the user signs over what was left out as well as
  what was written; a mined want the register already covers needs nothing. A
  line whose text actually changed in response to a finding gets a fresh
  hollow verdict before signature — re-run the hollow leg over just those
  changed lines, at most once per line per round; a verdict the re-run
  returns is closed by the same three dispositions, never by another re-run,
  which is what makes the audit terminate. When that verdict prompts a
  further sharpening, the further text gets no re-run either: close it as
  covered by siblings or as an accepted residual, recording your own
  whole-register read as the reason the final wording defeats — or
  accepts — the cheap reading. A
  degraded or partial audit (an engine down, a collapsed leg, a mining leg
  that twice returned zero wants) is never
  silently a pass; the Hard requirement block in
  [references/ensemble-workflows.md](references/ensemble-workflows.md) carries
  the ruling.
- **Updates clear the preservation passes before the read** — the shedding
  run's after-check, where a shed or material rewrite fired it: both reports are
  surfaced to the user, never collapsed to a pass, and any confirmed loss — or
  material loss the sighted sweep surfaced as an `additional_issue` — is
  reinstated into the draft, so the diff the user signs already includes the
  reinstatement. The passes run in both directions: an unsettled addition the
  gains direction surfaces is settled through the handshake or removed, and a
  collision is reconciled, before the read — a rule no exchange settled never
  rides into sign-off as prose. Each claim the gains direction surfaced and the
  round then settled is a mandatory decision card on the sign-off read — never
  judged minor and left off. Mechanics in
  [references/shedding-workflow.md](references/shedding-workflow.md).
- **Sweep `ISSUES.md` before the read.** A commission conversation parks its own
  tangential finds in the log — a bug or smell noticed in passing goes there like
  any agent's, under the annexe discipline, rather than derailing the intent
  work — so re-run the `ISSUES.md` triage (Reading the Situation) once more
  before sign-off. The sweep ends with the log drained: every entry — including
  the finds this pass's own grounding, surveys, and verifiers produced, which
  are triage input for this same pass, not material to log for a later one —
  leaves by a standard disposition, and an entry remains only when the user
  explicitly defers it. Report the log's state at sign-off as it is: "triaged"
  means drained. An intent
  realisation that surfaced mid-conversation is not one of these parked finds: it
  is the work itself, settled live through the handshake, never parked in the log.
- The final read checks **assembly** — interactions, omissions, emphasis —
  not items. Each item was already handshake-verified as it settled, so this
  is not the batched-verification anti-pattern: nothing is being verified for
  the first time at the end.
- **Approval is an explicit yes — the one ask, covering commit and push —
  recorded in the closing commit message.** The user's sign-off reply is that
  yes, unless its accompanying comments override it (the verdict handling in
  [references/signoff-page.md](references/signoff-page.md)). No status fields
  in the file; git is the audit trail. No machine-readable sign-off state:
  the user commissioning a run *is* the assertion that the contract stands.
- **Stop when:** the handshake has converged, the four jobs are quiet — no
  unverified load-bearing items, open challenges, or unresolved
  contradictions — devil's-advocate findings (where run) landed before
  the read, a blind re-derivation's divergences and proposed amendments
  (where run) were settled through the handshake rather than left as an
  unread comparison, no load-bearing inferred rationale remains in the contract
  unconfirmed and unmarked, the commitment audit (where it fired) has
  cleared — every in-scope line carries a hollow verdict (a null or missing
  verdict is a check still owed, never a pass), every hollow verdict closed by
  a sharpening, a covered-by-siblings record, or an accepted residual on the
  sign-off read, lines whose text its findings changed re-audited once, every
  exclusion-diff want dispositioned — every
  `stands on` entry resolves to another commitment or a verifiable existing
  capability, the preservation passes (where a shed or
  material rewrite fired them) have cleared with no unresolved confirmed loss,
  unsettled addition, unreconciled collision,
  or unaddressed sighted-sweep `additional_issue` — a gate the user explicitly
  chose to skip or degrade counts as cleared only when the sign-off report
  names it as such — `ISSUES.md` is drained — no entry remains without the user's explicit
  deferral — and, where this session changed the README, the tend sweep has
  run: every held file's pin advanced or its breakage raised
  ([references/held-intent.md](references/held-intent.md)). The surprise test bounds depth: remaining unknowns should be ones the user
  would genuinely leave to an implementer. Resist
  capture-completionism — an over-captured file is over-specification wearing
  a virtue's clothes.

## Session Transcripts

The commitment audit's miners read this conversation's own transcript from
disk — the falsifier against selective memory is the verbatim record, so the
audit takes file paths, never a summary of what was said. The transcript goes
verbatim to workers on both model families: everything pasted or read into
this conversation rides along, so flag anything sensitive to the user before
dispatching (registered secrets are already redacted from the record by the
platform).

The platform keeps every turn of this conversation in its local record,
and the `session_transcript` tool returns it: every user and assistant
message of this session, verbatim and in order, across resumes and
compactions. Call it with no arguments for this session from the start
(`offset: 0`); it returns `messages` — each `{ seq, role, text,
created_at }` — and, when more remain, `truncated: true` with a
`follow_up` naming this tool and the next `offset`, the way `docs_read`
pages a long document; call it again with that offset until no
`follow_up` is returned. Before dispatching the audit, read the whole
transcript that way and write it unchanged to files under `<scratch>` —
one JSON line per message as the tool returned it, one file per page —
and pass those file paths as the workflow's transcript arguments.

Never reconstruct the transcript: a file assembled from remembered or
selected user messages with the assistant's turns summarised is not the
record, and an audit over it checks the conversation against your memory
of the conversation. If the tool is unavailable or returns an error, the
audit cannot run — say so, record the round's audit as not run, and let
the sign-off read carry that fact as a decision card, rather than
substitute a reconstruction.

## Workspace

Dispatched-work output, staged options, and the directories carried
workflows write to live under `<scratch>`. The contract and everything
meant to persist live in the annexe repo: the contract at the root, and
this skill's own records — a sign-off report, an audit's verdicts — under
`commission/<date>-<round>/`, committed with the round. Nothing of this
conversation's goes under `build/`, whose presence the structural phase
reads as its own resume state; a commission that leaves records there
forces the next phase to ask the operator to move them.

`<scratch>` is the topic's scratch area — the session working directory
the runtime provisions when no workspace is attached; name a directory
under it per dispatch via the placement option (`cwd`).

## References

- [references/commitment-register.md](references/commitment-register.md) —
  the commitment line grammar (ID, consumer, act, falsifier), the stands-on
  gate, the reader-protocol block, the cheapest-compliant-implementation test,
  and worked contrasts. Read before writing or rewriting a What.
- [references/alignment-handshake.md](references/alignment-handshake.md) —
  the handshake protocol: invariants and per-level application notes, including
  how to assert an inference without attributing it to the user.
  Orchestrator-borrowable.
- [references/elicitation-moves.md](references/elicitation-moves.md) —
  probes for what the user has not said: journey walking, concrete-instance
  forcing, the disappointment and success probes, the why-ladder. Read when
  establishing or substantially extending the What.
- [references/misconception-moves.md](references/misconception-moves.md) —
  the dispatched hunting moves, their information diets, the reconciliation
  beat, relay discipline, and the Ensemble hard requirement. Read when a
  hunting signal fires and when findings return.
- [references/friction-signals.md](references/friction-signals.md) —
  corroborating a friction-ledger recurrence against the repository's own
  history: which structural signals to trust, which to reject, and what
  conjunction makes a refactor proposal earned. Read when `FRICTION.md` shows
  the same area recurring.
- [references/held-intent.md](references/held-intent.md) —
  held intent: the trust model, the held-file anatomy, and the hold, revise,
  tend, release, and withdrawal moves. Read whenever a hold, release, or
  withdrawal is asked for, or an update conversation finds `held/` non-empty.
- [references/signoff-page.md](references/signoff-page.md) —
  delivering the sign-off read: presenting the diff and its decision cards,
  waiting for the verdict, and revision rounds. Read at every sign-off.
- [references/dispatched-agents.md](references/dispatched-agents.md) —
  the dispatch doctrine: when to dispatch, grounding rules, the in-context
  verification move, and how findings come back grounded.
- [references/model-selection.md](references/model-selection.md) —
  pricing a dispatched worker's tier by its output's downstream life; why the
  workflows' drafting legs run the top tier while review legs deliberately
  don't.
- [references/effort-calibration.md](references/effort-calibration.md) —
  pricing a dispatched worker's effort by its undecided residue; the
  calibration behind the workflow scripts' pinned levels.
- [references/ensemble-workflows.md](references/ensemble-workflows.md) —
  running this skill's six carried cross-family workflows (divergent survey,
  blind re-derivation, draft review, grounding, commitment audit,
  preservation): invocation, the blindness discipline, the hard requirement,
  and the per-workflow brief each one needs.
- [references/shedding-workflow.md](references/shedding-workflow.md) —
  the shedding run on the update path: sweep, ground against the code, shed,
  then the blind and adjudication preservation passes, with triage and sign-off
  wiring.

