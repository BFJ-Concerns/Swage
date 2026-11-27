# Survey protocol

The survey gathers the evidence the writer works from. It produces working
notes and nothing that is committed.

## What grounds a claim

A claim is grounded by the thing that makes it true:

| Claim about | Grounded by |
|---|---|
| Behaviour | The implementation, and the tests that exercise it |
| Build, test and run commands | Manifests, CI configuration, and scripts that are present |
| Configuration | The config schema or parser, its defaults, and the environment handling |
| The command line | The argument parser, or `--help` output from a run |
| An API | Route definitions, an OpenAPI document, the handlers |
| Errors | Error enums, constants, and the paths that raise them |

CI configuration is worth reading early: it shows the build and test
commands the project really runs, which the README may not.

Where sources disagree, the code decides, then the tests, then CI
configuration, then examples, and existing documentation last. A
disagreement worth resolving in the documentation is usually worth a line in
the report too.

## The fact inventory

One row for each claim that matters:

```
claim | source (file:line, or the command run) | reader | home | checked against | what is unsure
```

- **Reader** is who acts on the fact: an agent, a person getting oriented, a
  person going deep, or a person operating a running instance. It decides
  the home.
- **Checked against** is what the accuracy review compares the claim with:
  `code`, or the path of the agent page that a human page's claim restates
  (`parallel-sets.md`, *The order of the accuracy review*). The writer fills
  this in. Survey workers do not report it.
- A fact true only on one machine or deployment says so in its row, so that
  the writer names that scope in the sentence.
- A row marked unsure is one the accuracy review must settle or the
  documentation must leave out.

Workers sent to survey an area return rows in this shape without the
checked-against column (`../prompts/survey-worker.md`).

A job on one README needs no formal inventory; notes are enough. A set of
several files needs the rows, because the accuracy review is run from them.

## Running commands

A command the documentation will quote is worth trying, within limits.
Read-only invocations, such as `--help`, `--version` or listing subcommands,
are fine. A build, a test suite or anything that changes state runs only
when it is already part of this session's work or the person asks for it. Do
not run a command whose effects are unclear in order to document it.

## A repository with several packages

Survey each package, and document at the level a reader arrives at. The root
README orients across the packages. A package that is used on its own
carries its own README and reference detail. Changes proposed for the
agent-guidance file are reported for the repository's one file, or per
package where packages already keep their own.
