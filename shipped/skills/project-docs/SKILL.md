---
name: Project Docs
description: Write or refresh a project's repository documentation — its README, a docs/ tree sized to the project, and the matching agent-facing set with generated llms.txt indexes — so that it is true to the code as it stands and is about using the product. Use when asked to document a project or repository, write or update a README or docs, create or refresh agent-facing docs or an llms.txt index, bring stale documentation up to date, or sync docs after a feature lands. Not for code comments or docstrings, changelogs, LICENSE or CONTRIBUTING files, setting up a doc-site tool, or work that is only on the project's agent-guidance file (AGENTS.md, CLAUDE.md).
---

# Project docs

The job is documentation a reader can rely on: every statement true of the
code as it stands, sized to what the project really has, and about using the
product: installing, configuring, running, operating and building it from
source. It has two readers. A person wants narrative, reasons and guides. A
coding agent wants terse fact it can act on. Each fact has one home, chosen
by who acts on it.

A request is one of two kinds, or both. **Writing** creates documentation
that does not exist. **Refreshing** checks existing documentation against the
code and corrects it. Most requests mix them: check what exists first, then
write only the files that are missing.

## What this skill does not write

- **How the project is developed**: contribution setup, development
  workflow, release mechanics, internal design and decision records. The
  documents here are for someone using the product, and development knowledge
  kept among them goes stale unnoticed because its readers are elsewhere. A
  request for such a document is ordinary writing work and not a job for this
  skill. On a refresh, development material found in the README, `docs/` or
  `agent-docs/` is listed in the drift report as misplaced, with where it
  could go, and then removed from those files unless the person wants it
  kept.
- **Root-level community and legal files** (`LICENSE`, `CONTRIBUTING.md`,
  issue and pull-request templates). Their terms are the owner's decision.
  Never create, edit or delete one; mention it in the report if something
  about it matters.
- **Plans**: roadmaps, task lists, wish lists. Found inside the docs, they
  are moved to wherever the project keeps its plans, or listed in the report
  where it has no such place. They are neither left in the docs nor dropped
  silently.
- **The agent-guidance file** (`AGENTS.md`, `CLAUDE.md` and files like them).
  It is instruction text with its own conventions, and a second author
  restructuring it causes the drift this skill exists to remove. See *The
  agent-guidance file* below.
- **Doc-site tooling**. Where a project already uses a generator such as
  mkdocs or docusaurus, follow its conventions. Do not install or migrate
  one.

Code comments, docstrings and changelogs are other work. Say so before
surveying anything.

## What makes the documentation reliable

**Document what is implemented.** A statement about behaviour, a command, a
setting, an interface or a dependency traces to a source read in this
session: the file that makes it true, or a command that was run. Plans,
TODOs, dead code, paths behind a disabled flag and the existing docs are not
sources, since the existing docs are what is being checked. A sentence with
no source is cut.

**A file exists because it has content.** A README alone is the starting
point and often the right size. A `docs/` page exists when there is a body of
grounded content today that fits nowhere else; ask whether deleting it would
lose information that exists now. Write no empty sections and no
placeholders. A person's request for a particular document settles that the
document should exist, provided there is real content for it.

**One home for each fact.** Exact values, tables and full procedures live in
one file, chosen by who acts on them: an agent's on-demand reference in
`agent-docs/`, a person getting oriented in the README, a person going deep
in `docs/` guides and reference, a person operating a running instance in the
runbook. Every other file carries a one-line pointer or a short excerpt with
a link, so that a change to the fact is one edit. Two things are not
duplication: the README keeps copy-paste install, run and test lines whatever
else holds the detail, because a front door made only of links orients
nobody; and a human page may restate its agent counterpart under the rules in
`references/parallel-sets.md`.

**On a refresh, correct the wrong statement and leave the rest.** The unit of
change is the claim that is wrong. Keep prose that is correct, with its
voice, terms and heading style. Rewrite a whole file only when corrections
cannot leave it readable, and say so in the drift report first. Read a file
before changing it.

**Reference rows are copied from their source.** Flags, defaults, types and
error codes written from memory are where documentation invents things. Each
row of a reference table comes from the argument parser, the config schema,
the error enum, generated metadata or the output of a command that was run,
in the code's exact spelling: where the code says `timeout_ms`, so does the
page. A row with no source is left out. A section a generator produces is
changed by changing the generator's input; where that is not possible, report
it.

**Say where a fact is true.** A statement that holds only on one machine or
deployment, such as a service unit, a deployment path or host-specific
setup, names that scope in the sentence. Write such a section only when this
session can check its claims on the machine they describe; otherwise leave it
out and name the gap in the report.

**Publish no secrets.** Surveying reads config files, environment handling
and CI settings. Documentation describes the shape of a credential setting,
such as an API key read from a named variable, and never carries a secret, a
token or a private hostname. A path that exists only on the machine the
survey ran on, such as this checkout's own location, is written relative to
the repository. The deployment paths of the paragraph above are the
exception, and name their scope.

## Two readers, two registers

Decide who a document is for before drafting it, because the two registers
fail in opposite ways and a compromise serves neither.

- **For an agent**: reference an agent looks up and acts on. Terse, one term
  per thing, each fact once, tables where the content is a list. No
  motivation and no walkthroughs.
- **For a person**: the README, guides, explanations and runbooks. Plain,
  specific prose that reads as written by someone who knows the product. Read
  `references/human-register.md` before drafting or substantially revising
  it.

After the content and structure are settled, go over the wording once for
clarity alone, changing no claim. Do this before the accuracy review, so that
the review checks the text that ships.

Once a project has a `docs/` tree, the two registers become two trees: `docs/`
for people and `agent-docs/` for agents, with the agent page written first
and the human page derived from it, and indexes generated by script. Read
`references/parallel-sets.md` whenever the project has, or this job creates,
a `docs/` tree.

Use plain GitHub-flavoured Markdown: relative links, one H1 title per
document, ordinary headings. Alert blocks (`> [!NOTE]`, `> [!WARNING]`) and
collapsible `<details>` sections are fine where the project's forge renders
them and the content is a real caution or bulky optional material. Use no
wikilinks, and no front matter beyond what `references/parallel-sets.md`
defines.

## Sizing the job

- A request about one file ("the README is stale") covers that file and the
  facts it shares a home with. A request about the whole repository covers
  the README, `docs/`, `agent-docs/` and what the agent-guidance file states.
- A request for a single new document settles its kind, reader, purpose and
  scope first. Ask when the request leaves one of them open, and show an
  outline before writing the whole.
- A small job is done directly: read the code behind the file, correct it,
  check it. The steps below are for jobs with several files or a whole
  survey.

## Doing the work

1. **Find what exists**: the documentation files, and the places the
   project's surfaces are defined (manifests, CI configuration, entry
   points, command-line, config and API definitions, guidance files).
2. **Survey the code** and build a fact inventory: each claim with its
   source, its reader and its home. Read `references/survey-protocol.md`
   before a survey of any size. The inventory is working notes. Keep it
   outside the repository's tracked files and do not commit it.
3. **Plan**: which files, with a line on what each will hold, and what is
   deliberately left out. For anything beyond a correction to one file, show
   the plan before drafting, so that the person can change it while that
   costs nothing. On a refresh the plan is the drift report, written before
   any edit.
4. **Draft or correct**, as one writer working from the inventory.
   `references/workflow-write.md` covers writing a new set,
   `references/workflow-refresh.md` covers refreshing one, and
   `references/document-shapes.md` gives the shape of each kind of page.
5. **Check**. Run the accuracy review below. Run
   `scripts/check-doc-links.sh` over the changed files for broken relative
   links, and `scripts/doc-indexes.sh --check` where the project keeps both
   trees. Look for exact values or procedures that now live in two files.
   Try documented commands where that is safe: read-only invocations such as
   `--help` freely; a build, a test suite or anything that changes state only
   when it is already part of this session's work or the person asks.
6. **Commit** the documentation changes, unless the person or the project
   holds commits.

## Workers

Reading can be shared out; writing cannot. A survey over many areas and the
checking of many claims are work for parallel workers. The drafting and the
decisions stay with one writer, so that the documents speak with one voice
and each thing is described once. Workers return evidence (a claim, its
source, what they are unsure of) and never prose to paste.

Use whatever the session has for this: a workflow where it can run one, or
its delegation tool, or neither. Surveying an area is bounded reading, which
a smaller model does well where the installation has more than one.
`prompts/survey-worker.md` and
`prompts/accuracy-review.md` are the workers' briefs. Send the text of the
brief followed by the area or the claim list, since a worker sees only what
it is sent.

## The accuracy review

Before the documentation counts as done, its claims are checked against the
code by a reader who has the claim list and the repository and has not seen
the writer's reasoning. That reader is a fresh worker where the session can
dispatch one, and on a different model family from the writer where the
installation has two, because a different family is less likely to share the
writer's misreading. With no way to dispatch, re-read each claim against its
source yourself after the drafting is finished. Say in the report which of
these the review was.

Each claim comes back confirmed, falsified or unverifiable. A claim that was
not confirmed is corrected from the evidence or removed. It does not ship
with a hedge or a flag. Where the project keeps both trees, the review runs
in the order `references/parallel-sets.md` gives.

## The agent-guidance file

A whole-repository job reads the agent-guidance file as it reads the other
documentation, and the survey will find facts that belong in it: the build
and test commands, the layout, standing constraints. It will also find
statements in it that the code no longer supports. Put both in the report as
proposed changes to that file, each with its source.

Edit the file only when the request covers it, and then only to correct
statements the code contradicts; its structure and its other content stay as
they are. Where the session has a skill for writing that kind of file, make
the edit with that skill. The guidance file carries what an agent needs in
every session and points into the README and `docs/` for the rest, so nothing
is copied between them.

## The report

Say what was created and changed. On a refresh, give the drift report. List
the claims removed or left out for want of evidence, the development and
planning material removed and where it went, the documents deliberately not
written and why, and the changes proposed for the agent-guidance file. Say
what checking was done and where it stopped: which kind of accuracy review,
which commands were not run, which areas could not be read. The drift report
belongs in the conversation or a pull-request description and is not
committed.

## References

- `references/survey-protocol.md` — what counts as evidence, the fact
  inventory's format, and which commands are safe to run. Read before a
  survey of any size.
- `references/workflow-write.md` — choosing the set of documents and shaping
  the README. Read when writing more than one file.
- `references/workflow-refresh.md` — auditing in both directions, the drift
  report, and how to correct. Read when refreshing.
- `references/document-shapes.md` — reference pages, guides and runbooks.
  Read when drafting one.
- `references/human-register.md` — the voice of prose written for people.
  Read before drafting or substantially revising it.
- `references/parallel-sets.md` — the `docs/` and `agent-docs/` trees, their
  front matter, the generated indexes and the order of the accuracy review.
  Read whenever the project has or gains a `docs/` tree.
