# Spine Authoring

Judging what the spine must cover, recording exemplars, and verifying the
spine by reading. Read this before the first exemplar of a phase.

## Judging Sufficiency

The spine is sufficient when every obligation the commission's remaining
work implies — including the restoration obligations this phase's own
teardowns seeded in the ledger — can name an exemplar its build unit would
extend. Work from the commission delta plus the ledger, not from the
codebase's own shape: list the kinds of thing the remaining work builds
(endpoints, services, migrations, UI surfaces, generators), and for each
kind ask whether a landed exemplar demonstrates the pattern a builder
would follow. A kind with no exemplar is spine work; a kind the
commission no longer needs is nothing. An obligation's `Must be true`
and `Proof` fields describe its later discharge by the assembly phase —
they are never acceptance criteria for the exemplar that covers it, and
a failing test an entry names is expected to stay red through handover.

Sufficiency is judged per kind, not per file. One endpoint demonstrating
the anatomy — routing, validation, error shape, persistence call, test —
covers every sibling endpoint a builder will make from it. Two exemplars
of one kind is usually one too many; prefer extending the exemplar's
declared range over duplicating it.

Know what already exists: before each record, dispatch cheap recon
(SKILL.md, Grunt Work Is Never Yours) across the landed tree and any
open unit branches to locate existing implementations of the covered
kind's helper surface. On brownfield, charter a duplicate into the
covering exemplar's landing for removal, or seed its own ledger
obligation where the strip outgrows the exemplar; either way, record in
the envelope the shared helpers a builder extends through rather than
re-creates.

The phase is allowance-bounded, so order spine work by leverage: the
pattern the most obligations will extend lands first. Allowance exhausted
before sufficiency is a pause, never a handover (SKILL.md, the handover
conditions): journal what is covered and what is not, leave the header's
handover ref `unset`, and resume when the allowance does.

## The Exemplar Record

One entry per exemplar in `build/spine.md`:

```markdown
## <pattern name>
- **Demonstrates:** what pattern this is the reference for, in one line.
- **Where:** the landed files (absolute repo paths) and the commit that
  landed them.
- **Extends at:** the envelope a builder works inside — where new sibling
  files go, the registration step, the test location, the incidental
  wiring (imports, config) the extension is expected to touch — concretely
  enough that a brief can say "extend <pattern name>" and the builder can
  tell expected wiring from trespass into unassessed code. Name here,
  by path, the shared helpers the pattern uses rather than owns:
  siblings call those, and copying the exemplar's private helper block
  into a new file is trespass, not imitation.
- **Imitation stops at:** the boundary past which this pattern must not be
  generalised, with the reason. This line is what keeps a meticulous
  builder from carrying the pattern somewhere it does not belong. It marks
  where a *good* pattern's applicability ends — it is never a fence around
  condemned code. Writing "known debt, do not imitate" here means the
  tear-out duty (SKILL.md, Brownfield) has already triggered: remove the
  code instead of warning about it.
```

The inventory is a whitelist read mechanically by the assembly's admission
rule, so keep entries current on every change: an exemplar reworked without
its record updated poisons every later brief that names it.

When writing an exemplar's tests, read the annexe's `VERIFICATION.md`
rung the pattern's descendants will be proven at — the assembly's
per-unit proof bar — and write test idiom a builder can extend into that
proof. A missing `VERIFICATION.md` is a contract gap, not authoring
work: record it in `build/gaps.md` and tell the user a commission
conversation owes the ladder. What "used as it would be used" means is
intent, and intent has one writer.

## Verification Is Reading

The spine's quality bar is reading (SKILL.md, The Spine), in three
layers:

1. **Your own read-back.** After landing each exemplar, re-read it whole —
   code and tests — as the builder will meet it: does it demonstrate the
   pattern cleanly, does the test idiom carry, does the envelope in its
   record match what the files actually show?
2. **A blind top-tier pass, by default.** Before handover, dispatch a
   same-family reader at the exemplar-authoring tier over the spine —
   the landed exemplar files plus `build/spine.md` — blind to the
   authoring story: no design rationale, no journal, just the code, the
   records, and the commission. A read is cheap even at the top tier, and
   the strongest reader available is the right one for the artefact the
   whole build will copy.
3. **A cross-family pass.** The same brief to a strong opposite-family
   reader, for the blind spots a same-family read shares with the author.
4. **A weak reader, when the phase authored instruction text.** Add a
   below-drafting-tier reader over the instruction-text files alone: its
   misreadings preview the weakest model that will execute the text —
   evidence the strong readers cannot produce. Its question is its own:
   not fitness to imitate but what it *understood* the text to ask of it,
   so its report shows comprehension gaps directly. Save and adjudicate
   its report with the others.

When instruction text is executed against a machine contract — an output
schema, a validated reply shape, a required field set — the contract is
part of what the text must satisfy, so name it as an input to every
reading of that text: check the text against it in your own read-back,
and point every dispatched pass at it, the weak reader included.
Text-vs-contract disagreement is then inside the bounded question:
fields the contract requires that the text never names, instructions the
contract cannot express, cases the text creates that have no valid
mapping — left unnamed, the contract is checked only by reviewer
initiative. Instruction text with no machine contract needs none of
this.

A dispatched reader receives only its brief, so every dispatched pass's
brief, the weak reader's included, states the phase's execution boundary:
judge by reading alone; run no build, test suite, type-checker, linter,
or other project toolchain command, even where the repository's own
guidance names one.

The two spine passes (2 and 3) get the same bounded question: is the spine
*wrong* — an exemplar that would mislead a builder imitating it, an
envelope that misdescribes the landed files, test idiom that would not
carry, a mechanical defect establishable from the text. Fitness to
imitate is the whole bar. Whether the code could be more robust, more
general, or more complete is not the question: state in the brief that
findings proposing capability, hardening, or design beyond the
commission's remaining work are out of scope — the phase's product is
the minimum sufficient spine, and review pressure to grow it is how the
minimum dies.

Save every dispatched pass's report under `build/reviews/` and adjudicate every finding
before handover: fix it, or journal the reason it is rejected. The
readers advise; the seat decides.

Read at the pattern's edge, not just its centre: the error path, the
empty case, whatever the exemplar's boundary line says a builder will hit
first. The exemplar is copied at volume, so the scrutiny it gets here is
the only scrutiny most of its descendants will honestly inherit.
