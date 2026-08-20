# Friction Signals

How to corroborate a friction-ledger recurrence against the repository's own
history, and which signals deserve trust. The governing observation: rot shows
as a *shape* — a structural pattern or a recurrence pattern — never as a
volume. Volume measures (how much changed, how big, how often) discriminate
nothing in agent-driven development, which is high-volume by nature. Every
check below therefore compares the suspect area against the repository's own
baseline; absolute thresholds imported from someone else's project mislead in
both directions.

## Signals Worth Trusting

- **Dispersed co-change without dependency.** Files that repeatedly change in
  the same commits yet share no import or include relationship — especially
  across module boundaries — indicate a hidden coupling the architecture does
  not admit. List an area's co-change partners with
  `git log --name-only --pretty=format: -- <path>` piped through
  `sort | uniq -c | sort -rn`, ignoring bulk commits that touch scores of
  files; partners that rank high with no dependency link are the finding. A
  hub whose partners are unrelated to *each other* (a star, not a cluster) is
  the worse shape.
- **Fix breadth and fix recurrence.** Fixes landing in the area touch notably
  more files than the repository's typical fix, and the same files keep
  reappearing in fix commits month after month. Deep-but-narrow fixes are
  merely hard work; wide-and-recurring fixes are structure.
- **Fresh-line rework.** Lines authored recently that are already being
  rewritten. Take a fully elapsed window (rework in the last fortnight is
  still accumulating and reads falsely low) and compare against the
  repository's own trailing norm; a rising rework rate on new lines survives
  the agents-are-churny objection because it is a rate per authored line, not
  a volume.
- **Oscillation.** Behaviour flipped and then unflipped; code removed and
  later reintroduced; a setting toggled back and forth across commits. Read
  the area's log for reversals — no threshold needed, reversals are the
  signal at any rate.
- **Non-converging work.** Across successive sessions on the same goal,
  finished state does not accumulate: each pass replaces the last's fixes,
  "almost done" holds constant, and — the surest single sign — the stated
  rationale for the work keeps changing. Fresh sessions are especially prone
  to re-believing "nearly there"; the ledger and the task history are what
  make the non-convergence visible.
- **Clone drift.** A change lands in one copy of a duplicated block while its
  siblings go stale. The rot is the non-co-evolution, not the duplication
  itself.

## Signals to Reject

- **Total churn volume** — inherent to agent-driven development; measures
  activity, not health.
- **Raw complexity of a file** — may be irreducible domain logic; complexity
  matters only in conjunction with recurrence and trend.
- **Presence of TODO/FIXME admissions alone** — age, density on the change
  path, and trend carry some weight; mere presence carries none.
- **Any absolute threshold imported from another project or dataset** — every
  usable check is a comparison against this repository's own baseline.

## Reading the Result

One trustworthy signal is a lead worth a second look. A case worth a refactor
proposal is a conjunction: ledger recurrence *plus* at least one structural
pattern from the history *plus* a trend that is worsening rather than stable.
When the evidence stops at "this area is unpleasant but stable and shallow",
record that conclusion in the ledger too — a checked-and-cleared entry stops
the same lead being re-investigated next quarter.
