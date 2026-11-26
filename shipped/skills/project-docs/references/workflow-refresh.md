# Refreshing existing documentation

A refresh delivers a drift report and the corrections it lists. It is not a
rewrite.

## How much to audit

- **After particular changes** (some commits, a pull request, a feature):
  read the diff, group what changed into areas, and for each note which files
  changed, what changed in behaviour, and which documents could be affected.
  Only those documents are touched.
- **The whole repository** (a full audit, a first pass over inherited docs,
  after a large refactor): find the project's main areas and audit all the
  documentation for each.

An area is one coherent piece of what the product does, named as a user
would name it where that is possible. Each kind of interface (an API, a
command line, a config file) is usually an area of its own.

## Audit in both directions

Checking only what the documents say finds half the drift.

1. **From the documents to the code.** Sort each claim that matters: still
   true, out of date, false, unsupported, stated twice, or in the wrong
   place. `survey-protocol.md` says what grounds a claim.
2. **From the code to the documents.** Each implemented surface a user
   meets either has a documented home or is a gap.

## The drift report

Write it before editing anything. It is the plan the person can check, and
the point at which they can keep something that would otherwise be removed.
It lists the mismatches that matter, each as one of:

- **Misdescribed**: the document says one thing and the code does another.
  Cite both.
- **Gap**: implemented, needed by a reader, and not documented.
- **Dead**: describes code that no longer exists.
- **Misplaced**: accurate, but not documentation of using the product:
  development or planning material. Name it, where it is, and what will
  happen to it: removed, moved, or passed on in the report.

## Correcting

- Make the smallest edit that makes the claim true, and leave correct prose
  as it is.
- A whole file is rewritten only when corrections cannot leave it readable,
  with that reason given in the drift report first. A document that mostly
  describes intentions is such a case.
- **The same fact in two places, now different**: the code says which copy
  is right, and who acts on the fact says which file keeps it. The other copy
  becomes a pointer, and links to it are repaired in the same pass.
- **A dead document is deleted** and noted in the drift report, since a page
  describing removed code misleads more than a gap does.
- **Misplaced material** is removed from the README, `docs/` and
  `agent-docs/` pages the job covers, once the drift report has listed it and
  the person has not asked to keep it. Plans move to where the project keeps
  its plans. Where such material turns up in a file outside the job, or in a
  root-level community or legal file, report it and leave the file alone.

## Where the project keeps both trees

Audit both (`parallel-sets.md`). Correct the affected `agent-docs/` pages
against the code first, then re-derive the human pages that restate them.
Pages that exist only for people are corrected directly. Finish by
regenerating the indexes with `scripts/doc-indexes.sh`: a page added, removed
or retitled without its index entry fails the check.

## How much the accuracy review covers

Every claim added or changed is reviewed. In a very large set, where
reviewing the unchanged claims as well would double the cost, review a sample
of those and state the coverage in the report.
