# The Commitment Register

The What's binding core: a flat set of numbered commitment lines, each one
naming who can do what and how its absence would show. The register exists
because prose destinations fail in a specific way — quality-language a lazy
implementation satisfies, commitments invisible under the wrong heading,
wanted capability that never became text — and each rule here targets one of
those routes, making the failure visible and checkable rather than
impossible. Read this before writing or rewriting a What.

## The Commitment Line

```markdown
- **C7** (lead agent) can compose a sub-agent's briefing from named, reusable
  context elements at dispatch time; absent when every dispatched child's
  briefing is a single hand-written prompt string. — stands on C5.
```

Four parts, all required:

- **A stable ID.** `C<n>`, unique for the life of the project, never renumbered
  and never reused — downstream mappings key on it. Goal lines take `G<n>`
  from their own series under the same rules (Goal Lines, below). The README is the sole
  allocator: a new ID is the next above **its own series'** high-water mark
  across the live
  register and the retired entries in `shed/decisions.md` (held bodies carry
  no IDs until release stamps them — see Register Upkeep). A descoped
  commitment's ID
  retires with it (through the same handshake as a Decision retirement).
- **A named consumer.** The party whose capability this is: a person, an agent,
  a service, a subsystem, an operator. Naming the consumer is what forces
  internal and agent-facing capability into the same checkable set as visible
  surfaces — and the falsifier must be an observation *that consumer* could
  make at its own boundary.
- **An observable act.** What the consumer can do, in the journey-flavoured,
  intent-level terms the writing rules already require.
- **A falsifier.** `absent when <one concrete observation>` — the observation
  that would show the capability missing. For prohibitions and invariants the
  clause is `violated when <observation>` and the act names what must hold.
  The falsifier is written at capture, while the person who knows the intent
  is in the room — reconstructing it at build close is how a hollow
  implementation passes.

Elaboration sits under the line it elaborates — the same intent-level journeys
as ever, contextualising its commitment. Elaboration binds as the *reading* of
its line — it constrains how the commitment is satisfied — but prose creates
no commitment on its
own: a behaviour found only in prose, under no line, is a capture defect (the
audit's
exclusion diff exists to catch it), so when elaboration grows a new claim,
give the claim a line. The one deliberate exception is a non-binding tail
mention (below).

Attributes trail the line after an em-dash, semicolon-separated: `— stands on
C5; after C3`. `stands on` is the one binding attribute (its gate below);
`after C<n>` and any horizon note are advisory ordering for the build, carrying
no scope semantics either way.

## No Heading Carries Scope

Sequence, phase, and distance from the present are attributes of a commitment,
never sections of the document. A committed capability sits in the register
whatever its build order — there is no "later" section whose heading a scoping
reader can take as out of scope, because a heading that carries scope
semantics is invisible to readers who source work item by item. A sequencing
note on a line is advisory ordering for the build; commitment status comes
only from the line existing.

The aspirational tail — material the user wants recorded but has not decided
should be built —
stays out of the register entirely: one-line non-binding mentions, `IDEAS.md`,
or `held/`. The register holds only what the user wants built, which is what
makes "every line maps to owned work" a check the build phases can actually
run — and a tail mention is one of the exclusion diff's standard dispositions,
so a voiced want can land there deliberately rather than being pressed into a
commitment.

## Stands-On: Dependency Is Register Material

A commitment that structurally rests on other capability carries a trailing
`stands on` attribute naming what it rests on — a permanent, intent-level fact
about the destination, not a build-progress marker: the attribute is written
when the dependency exists in the design and stays after the substrate is
built, so the contract never goes stale from work being done. Every entry
resolves to another
commitment ID or to an existing, verifiable capability — and that resolution
gates signature: a committed outcome standing on substrate that is neither
built nor itself committed is not signable. The repair is one of three, settled
with the user: commit the substrate (its own line, its own falsifier), hold the
outcome (`held/`, until the substrate is committed), or descope it. Ordering —
which is built first — stays runtime business; the dependency itself is
contract material, visible to every scoping read, because a dependency that
lives only in ordering advice is one no phase ever owns.

This is also the admission test for internal capability: a thing earns a line
when the user named it as wanted, or when a committed outcome stands on it and
it does not yet exist. Anything below that bar is implementation freedom —
leave it unwritten, per the writing rules' silence principle.

A register rewrite — a migration from prose, a shedding compression, a
restructure — preserves every dependency edge: a source clause that declares
one ("builds on X", "requires the Y substrate") survives as a `stands on`
entry on the successor line, or its dropping is surfaced at sign-off as its
own named change. The blind enumeration check that guards a rewrite
compares edges as well as IDs: dispatch a cheap worker the bare
before-and-after lists — the IDs, and the `stands on` pairs — and take
back only the symmetric differences, because a checker handed the
rewrite's reasoning rationalises a mismatch, and an ID-only diff reads a
register that kept every line and lost its structure as intact.

## Goal Lines: Milestones Are Contract Material

A milestone the user names as an aim — "dogfooding", "MVP", "launch-ready" —
is a commitment like any other, and it enters the register as a **goal
line**: the same four-part grammar, where the act is the end-to-end journey
the milestone means to the user, and the falsifier is an observation at the
running product's own boundary — the assembled, installed product as a close
walkthrough drives it, judged by what a fresh consumer with no build
context would find missing. A goal line names the
commitments it rests on in `stands on`, so the milestone's substrate is
readable from the line and gated exactly as any other dependency — and a
later commitment the milestone turns out to rest on joins that `stands on`
set in the same edit that lands it. In build mappings a goal line resolves
through its `stands on` set plus the closing walkthrough of its falsifier;
it never spawns an umbrella obligation duplicating its substrate.

```markdown
- **G1** (operator) can hand the product to a first paying customer:
  sign-up, first document, share, and billing each work from the public
  surface; absent when a first-time operator — a fresh agent standing in
  for one, driving the running product — cannot
  complete one of those journeys unaided. — stands on C3, C9, C14.
```

A goal line is a commitment line in every rule this file states — sweeps,
audits, the stands-on gate, shedding exemption, descoping — with its own
`G<n>` ID series under the same allocator, so a scoping or closing reader
can tell the destination's milestones from its capabilities at a glance.
Two rules make it load-bearing rather than decorative:

- **The word binds to the line.** Once a milestone has a goal line, every
  use of its name — a readiness question ("does anything block
  dogfooding?"), a horizon note, a scoping answer — resolves to that line
  and its falsifier, never to a heading, a section's contents, or a
  reader's sense of the word. A heading may borrow the milestone's name
  for navigation; it still carries no scope.
- **Capture fires on the voiced aim.** When the user states an aim the
  work is running toward, that statement is a goal-line capture moment:
  write the line then, falsifier included, while the person who knows what
  the milestone means is in the room. An aim that stays prose is the
  precise failure this construct exists to prevent — every downstream
  instrument verifies the register, so an aim outside it is invisible to
  all of them, and a run can verify green while missing the point.

The cheapest-compliant-implementation test bites hardest here: a goal
falsifier that one thin workflow or a demo path satisfies while the
capability the user means is absent is too weak. Sharpen it by naming the
parity that matters — what the milestone must match, replace, or make
unnecessary — rather than a count of features.

## Observability Is Tested at Capture

A falsifier must name an observation its consumer could actually make —
judged against the committed destination, not the current tree: a substrate
commitment whose consumer is itself committed-but-unbuilt is observable
enough, since the walk at close runs in the assembled product where both
exist. When no
discriminating observation can be found even in that destination — the
capability's absence
would be invisible at every boundary — the commitment is not settled enough to
sign: sharpen it with the user, or hold it. Discovering unobservability at
build close instead produces a verdict that can only be `inconclusive`, and
inconclusive blocks close — so the cheap moment to fix it is now.

At close, each commitment is judged against its falsifier as **present**,
**absent**, or **inconclusive** — three values, no coercion: a capability with
no obtainable evidence is inconclusive, never rounded up to present.

## The Reader-Protocol Block

The README carries this short normative block adjacent to the register, so a
reader that arrives without the build skills still receives the rules the
register depends on:

```markdown
> **Reader protocol.** The numbered commitment and goal lines are the binding
> destination, wherever they sit on the page; no heading narrows their scope,
> and a milestone's name resolves to its goal line, never to a heading. A
> reader deriving work maps every commitment ID to owned work or names the
> unmapped remainder explicitly. A reader judging completion produces evidence
> against every falsifier — present, absent, or inconclusive — and treats
> absent or inconclusive as blocking.
```

The block is contract machinery, not shed-able elaboration: it is exempt from
shedding and minimisation culls, and leaves the README only by explicit
retirement through the handshake — the same protection a Decisions entry
carries.

## The Cheapest-Compliant-Implementation Test

For each line, ask: what is the cheapest implementation that satisfies these
words exactly? If that implementation is hollow — it delivers the letter while
the capability the user wants is absent — the falsifier is too weak: sharpen
the `absent when` clause until the hollow version fails it. This is the
capture-time reflex; the commitment audit (`workflows/commitment-audit.js`)
runs the same test as recorded per-line verdicts before sign-off — judged
against the whole register, so a gap one line leaves that another line's
words close is not hollow — and a
missed reflex becomes visible rather than silently absorbed.

## Worked Contrasts

**Human-facing.** Weak: "search feels fast and relevant." Cheapest compliant:
any search box — no observation distinguishes it. Repaired: `**C3** (user) can
find a note by a phrase from its body within the first screen of results;
absent when a body-phrase query returns no match for an existing note.`

**Agent-facing.** Weak: "sub-agent context is assembled from stable grounding
elements." Cheapest compliant: concatenating the previous report into the
prompt string — "elements" satisfied by string-building. Repaired: `**C7**
(lead agent) can compose a sub-agent's briefing from named, reusable context
elements selected and parameterised at dispatch time; absent when a child's
briefing cannot be produced except as a hand-written prompt string.`

**Internal.** Weak: "runs are durable." Cheapest compliant: a log file nobody
replays. Repaired: `**C11** (workflow engine) can resume an interrupted run
from its journal without re-executing completed steps; absent when killing a
run mid-step and restarting re-runs a completed step or loses the run.`

## Register Upkeep

- **Shedding.** A commitment line is already the compressed form shedding aims
  at: once the build embodies it, shedding compresses the *elaboration* around
  it (to `shed/`, as ever) and the line stays — still true, still the
  acceptance set for every later run.
- **Descoping** a commitment is an intent change through the handshake and
  sign-off. Append the retired line — text, ID, reason, date — to
  `shed/decisions.md`, the same append-only ledger retired Decisions use: that
  is the durable record a later reader needs to tell a retired ID from a
  numbering slip, and the high-water mark the ID allocator reads.
- **Held bodies carry no IDs.** A held file writes full commitment lines
  unstamped — a held goal line as `**G?**`, keeping its kind visible —
  and release allocates the next free IDs from each line's own series as
  it splices; stamping IDs
  is not a redraft, so the splice guarantee holds and held files can never
  collide with a register that moved while they waited.
- **Collision checks at the pen** now include the register: a new line is read
  against the standing lines and their falsifiers, not just the Decisions.
