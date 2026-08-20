# Held Intent

Sometimes the user wants a feature nailed down now — planned to full contract
rigour — but not built yet. Such intent has a home of its own: the annexe's
`held/` directory, one file per held feature. Writing it into the README would
authorise building it; leaving it in `IDEAS.md` would deny it the planning
rigour and a durable place to develop. `held/` is the third state: **verified
but not authorised**.

## The Trust Model

Build authority is membership in the README, and nothing else. A builder's
intent input is the README alone; agreement, completeness, sign-off history,
and the word "held" confer no authority, and no marker inside the README is
needed to protect held intent — the contract's core property, *everything in
it is fair game*, stays literally true with zero exceptions. Safety comes from
what a builder is given, never from a label it must notice: a skimmed-past
marker is the failure mode this shape exists to avoid.

Each annexe artefact's standing is one line:

| Artefact | Verified? | Authorised? |
|----------|-----------|-------------|
| `README.md` | yes — every item handshake-verified | yes — everything in it may be built, in any order |
| `held/` | yes — same handshake, same sign-off | no — released only by a commission conversation |
| `shed/` | formerly current — claims staleness | n/a — historical record; the README wins |
| `IDEAS.md` | no — raw capture | no |

The single-writer rule extends to `held/` unchanged: only this skill writes
it, and every entry and substantive change passes the handshake and lands via
a sign-off commit. One delimited mechanical exception exists — the tend
sweep's pin advance (below), which changes no meaning. Held intent differs
from the contract in exactly one property — the authorisation bit — and that
bit is the file's address. A cheaper write path into `held/` would be the
side door the single-writer rule exists to close.

One honest limit: the boundary is a read contract, not a capability wall. A
build agent *could* open `held/`. Where the tool injects guidance at
touch-time (on Claude Code, the global annexe-discipline hook), the read
contract is restated the moment an agent edits annexe files; on tools with no
such channel, each file's banner is the only in-band defence. If build output
is ever found implementing held content, raise it with the user rather than
absorbing it as a bug to patch: something the contract never authorised
reached the build, so what else the run took from `held/` is open, and the
user decides whether the work stands, is reverted, or is released properly.

## Anatomy of a Held File

One file per held feature: `held/<feature-slug>.md`, the slug derived from
the feature itself. The file carries, in order:

1. **The banner** — a fixed first line, verbatim:
   `**HELD — NOT AUTHORISED FOR BUILDING.** Not part of the contract; build intent comes from README.md alone.`
2. **The pin** — `Verified against contract: <blob-hash>` — the README's git
   blob hash (`git hash-object README.md`) at the last check. Content, not
   commit, so it is computable from the draft before anything lands and moves
   only when the contract's text moves. The file never claims currency, only
   "checked as of"; pin ≠ the current README's blob is a mechanically
   checkable staleness signal, the same honest posture `shed/` takes.
3. **The body** — written in the contract's own register: full commitment
   lines with consumers and falsifiers, elaboration, boundary statements,
   decisions where earned, all the writing
   rules of `SKILL.md` — with one deliberate difference: commitment lines in
   a held body carry **no IDs** (the README is the sole allocator, and the
   register moves while the file waits; release stamps the next free IDs as
   it splices, which changes no words). The body is otherwise the text the
   README will eventually
   receive, so release is a splice, not a redraft.

The pin attests a *check*, at two strengths: a handshake (entry and revision
verify the body with the user) or a tend judgement (the sweep judged an
intervening contract change disjoint from this file). Neither strength proves
semantic validity against everything the contract may since imply — that is
what release's unconditional revalidation is for.

No priority, ordering, dates, estimates, or progress — the format cannot
express a queue, so `held/` cannot decay into a backlog or a roadmap. What is
held is *agreed but withheld*, nothing more.

## Entry — the Hold

Fires when the user wants a feature settled but explicitly not built ("nail
this down, but don't work on it yet"). The conversation is a full commission
conversation for that feature — handshake, elicitation, misconception hunting
and surveys where their signals fire — with one difference in destination:
the settled text lands in `held/<slug>.md`, not the README.

- The rigour gate is real: material below contract grade is not held — it
  stays in `IDEAS.md` until a conversation can plan it properly. `held/`
  demands the same completeness the README does, or it becomes a second,
  laxer contract.
- The working tree is the draft, exactly as for the README: the file settles
  through the handshake as uncommitted edits, the user reads it at sign-off,
  and the sign-off yes lands the commit (and covers the push). The pin is the
  blob hash of the README the hold was verified against.
- The user signs off two things at once: the feature's substance, and its
  continued withholding.

## Revision — While Staying Held

Fires when the user wants to change a held feature's agreed substance without
releasing it. Edit the file in place — the working tree is the draft, as
ever — through the same machinery as entry: handshake the changed meaning,
land it with a sign-off commit, stay in `held/`. If the pin is stale, reconcile
first: walk what the contract has changed since the last check and fold the
consequences into the revision, so the new pin (the current README's blob)
is honest. A revision is a substantive change — it always takes the
handshake; only the tend sweep's pin advance is mechanical.

## Tend — the Sweep on Every Contract Change

A store no builder reads generates no natural pressure to stay fresh, so
freshness rides the one event that can create drift: the contract's own write
path. Whenever an update conversation lands a README change, sweep `held/`
before the sign-off read:

- For each held file, check the README diff against what the file assumes.
  Untouched → advance its pin to the draft README's blob (the delimited
  mechanical edit — it records only that this change was judged disjoint,
  no re-handshake). Touched → raise it with the user in the same
  conversation: re-verify the affected parts (a handshake) or leave the file
  honestly stale at its old pin.
- The sweep is judgement over a diff, not a dispatch — proportionate to the
  handful of files `held/` should hold.
- Read `held/` at the start of every update conversation as part of reading
  the README: a fresh session that does not know a feature is already held
  can re-plan it into the contract, and the collision surfaces only here.

## Release — into the Contract

Release is an explicit user act in a commission conversation — "we're ready
to build X now" — never inferred from enthusiasm, from a builder's question,
or from the feature being discussed. Pointing an orchestrator at a held file
is not a release; builders take intent from the README alone, so the route
into the build runs through this conversation or not at all.

A release is its own narrow move, not a full update conversation: it needs
the current-clone gate, the steps below, and the tend sweep for the *other*
held files (the splice changes the README, so the sweep fires as usual) — not
the standing queue triage, which belongs to genuine update conversations. A
release that also changes other intent has become an update conversation and
takes on the whole update machinery.

1. **Revalidate unconditionally.** Walk the held body against the current
   contract and re-handshake it with the user. The pin and the diff since it
   direct attention — a stale pin adds reconciliation work — but a current
   pin never waives this gate: the body was verified when held, and this
   re-read is what catches the semantic drift no pin can.
2. **Splice.** Merge the body into the README (banner and pin stay behind),
   stamping each commitment line with the next free ID, resolving placement
   and any collisions with existing contract text, and re-running the
   stands-on gate over the spliced lines — a dependency that resolved when
   the feature was held may have been descoped since, and an unresolved entry
   blocks the release exactly as it blocks any signature.
3. **Land one sign-off commit** that both edits the README and deletes the
   held file — the diff the user reads shows both, and no committed state has
   the feature in two places or none. Git history keeps the hold's
   provenance.

## Withdrawal — the Exit That Isn't Building

A held feature the user abandons is deleted through the same ceremony: an
explicit user decision, a sign-off commit that removes the file, and a
Decision in the README only where the threshold is met (multiple viable
options, costly to reverse, a future reader would ask). Git history preserves
what was agreed and when; `shed/` is not involved — it archives detail the
build has embodied, and a withdrawn feature was never built.
