# Writing a new set

Two decisions are particular to writing documentation that does not exist:
which documents to write, and how the README opens the project.

## Which documents

The set follows the content the survey found. As a guide to where projects
usually land:

- **A script or single-purpose tool**: a README.
- **A small tool or library**: a README, and one reference page where there
  is a real surface to list, such as a command line or a config file.
- **A service, an application, or a library with a real surface**: a README,
  then guides, reference pages and a runbook, each added when there is
  grounded content for it that fits nowhere else.

A request from the person for a particular document outranks this guide where
there is real content for it. Nothing here justifies an empty file.

Whenever the set includes a `docs/` tree, even a single reference page, the
`agent-docs/` tree comes with it (`parallel-sets.md`), and drafting ends by
generating the indexes with `scripts/doc-indexes.sh`.

Where the repository already has documentation conventions (a `docs/`
layout, a doc-site tool, a heading style), follow them in place of the
defaults here. This skill adds to a system that is there; it does not replace
one.

Existing docs and any Markdown the person supplies show the project's tone,
terms and conventions. Their sentences are not material to copy into new
documents unless the person asks for that.

## The README

The README is where a person starts. It has these parts, less any the
project lacks:

- what the project is and what it is for, in the first paragraph, in plain
  words before any jargon;
- how to install or build it, and a short start with commands that can be
  pasted and that work, copied from their source and tried where that is
  safe;
- one example of the main thing a person does with it;
- the few settings everyone needs, with a link to the full reference;
- links to the deeper documents, each reachable from the README directly or
  through one other page, and the project's status or support expectations
  where it states them.

Before finishing, check that purpose, install, build, test, run,
configuration and the main uses are each present or do not apply.

## Where facts go

- **Building from source and running the tests** are things a user does. The
  commands go in the README's install and build lines, with the test command
  beside them as the way to prove a build, and in a how-to guide where the
  project has earned one. Put with them whatever the environment needs that a
  reader would not assume: the toolchain, the platform, system libraries.
  Changing the test suite is development and is not documented.
- **Operating a running instance** goes in the runbook
  (`document-shapes.md`, *Runbooks*).
- **What an agent looks up when it needs it** (reference detail, interfaces,
  configuration, behaviour) is written in `agent-docs/`, and the human pages
  derive from it (`parallel-sets.md`).
- **What an agent needs in every session** (the build, test and run
  commands, the layout, standing constraints) belongs to the agent-guidance
  file and is reported for it, not written into the docs trees (the skill's
  *The agent-guidance file*).
