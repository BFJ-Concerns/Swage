# Teardown

The brownfield procedure: sweeping an area before condemning it, recording
what its removal displaces, and the closing audit. Read when first judging
whether an area falls short: this machinery makes condemnation affordable,
and judging without it biases the call toward keeping what should go.

## Before the Tear-Out: the Capability Sweep

Before removing or gutting an area, dispatch a cheap opposite-family worker
to sweep it and list every capability it provides — features, behaviours,
entry points, side effects other areas depend on — with a file/symbol
pointer per item. The sweep's output is durable state: save it as
`build/teardowns/<slug>.md`, opening with the area's paths and the commit
hash swept (the audit and any resumed session read these; memory does
not survive a session). The sweep is evidence-gathering, not judgement:
you decide what the list means; the worker only makes it complete. Sweep
before condemning, not after — once the code is gone, the only account
left is the record.

## Recording Displacement

Every capability the sweep lists is dispositioned at tear-out time, in the
same working session as the removal — never batched for later recall:

- **Restoration obligation** → a record in `build/obligations/<slug>.md`,
  in the shared format (references/build-ledger.md): the
  capability, stated as what must be true when it exists again; the proof
  it will require (its `VERIFICATION.md` rung); and its Source line
  pointing at the swept
  record of what it used to do — the closing audit's scope discipline
  keys on that pointer. Restoration means the capability exists
  again, proven — not that the old code returns.
- **Retired** → an entry in `build/retired.md`: the capability, the reason
  it is not coming back, and the authority — your judgement where the
  commission is silent, the user's explicit say-so where the commission's
  What names or implies it. When in doubt whether the commission speaks to
  it, ask; a capability silently dropped against the contract is the
  failure this whole file exists to prevent.

The obligation records and retired file together must account for the sweep's whole
list. An unaccounted capability is not a small omission — it is the exact
gap the closing audit will flag, so resolve it now while the context is
warm.

## The Closing Audit

Self-authored accounts drift flattering, so the phase does not close on
your ledger alone. Run the carried `teardown-audit` workflow before
handover: it ships with this skill, starts by name like any workflow,
and takes its inputs as workflow args (the script's header comment
names them; the run's result and journal carry the outcome back).

The args name the repository, the audit window's base
commit, the paths to the obligation records (`build/obligations/`),
`build/retired.md`, and the
`build/teardowns/` directory, and the output path for the report. Two
rules make the audit mean something:

- **The base commit is the ledger header's phase base** on a first audit;
  a re-entry visit that tore code out audits its own window — from the
  previous audit's recorded head hash to now — so already-adjudicated
  teardowns are not re-litigated and new ones are not missed. The window's
  head is resolved to a **commit hash before the run** and passed in the
  args; the report header records both hashes, which is what makes the
  next window's base recoverable from the artefacts. Run the audit from a
  checkout of the tree being handed over (under PR mode, the structural
  branch). A re-entry that removed nothing owes no new audit — journal
  that instead.
- **The audit checks the tear-out accounts, not just net loss.** The
  adjudicator reads the per-teardown sweep records alongside the diff, so
  a capability torn out, recorded, and already restored still gets its
  accounting checked — net-invisible is not audit-exempt.

The workflow: cheap opposite-family workers diff the window and write
their own removed-capability list blind to your records; an adjudicator
compares it against the obligation records, the retired file, and the sweep records,
and writes the report to the named output path — save it under
`build/audits/<date>.md` and put that path in the ledger header's
`Last audit` line. **The audit fails closed:** if no sweep worker returns
a usable list, the workflow errors rather than adjudicating an empty
account — rerun it; a phase whose audit cannot run does not hand over on
the strength of silence. A phase that tore nothing out records
exactly that: `Last audit: not applicable — no teardowns`, which the
assembly's reconciliation accepts in place of a report path only while
`build/teardowns/` is empty; once any teardown exists, the line must name
a report whose window reaches past that teardown.

Every discrepancy is resolved before handover — a missed capability
becomes a restoration obligation or a retirement (with the user's say-so
where the contract speaks), a contradicted record is corrected, and the
resolution is journalled beneath the audit report. A clean audit is part
of the handover artefact set; an unresolved discrepancy list is not a
handover, it is the phase still open.
