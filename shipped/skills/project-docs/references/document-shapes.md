# Document shapes

What a page of each kind holds, once the content for it exists. A section
with nothing grounded to say is left out.

Choose the kind by what the reader wants: to learn (a tutorial), to get a
task done (a how-to guide), to look something up (reference), or to
understand (a conceptual guide). Each kind is about using the product, so
there is no shape here for architecture, decision records or development
documents.

## Reference

Reference pages live in `docs/reference/`, one file for each surface the
project really has:

| File | Holds | Columns |
|---|---|---|
| `cli.md` | Commands, subcommands, flags, arguments | Flag, Short, Default, Description |
| `configuration.md` | Options, their types, defaults and valid values | Option, Type, Default, Description |
| `api.md` | Endpoints, methods, parameters, responses, errors | Parameter, Required, Type, Description |
| `errors.md` | Error codes, their meaning, cause and remedy | Code, Name, Description, Resolution |
| `glossary.md` | The project's terms and abbreviations | Term, Meaning |

A reference page lists every entry its source has, each copied from that
source. Put names and values in backticks, and break a long table up under
subheadings.

Where the project generates its reference (OpenAPI, TypeDoc, rustdoc and the
like), the generated output is the reference. Link to it and add what a
generator cannot say: why and when someone would use an endpoint or option.
Do not copy it out by hand.

## Guides

Guides live in `docs/guides/`. A **tutorial** teaches a newcomer by taking
them through one thing from start to finish: what they will learn, what they
need first, the steps in order, what they now know, where to go next. A
**how-to guide** gets a practitioner through one task and can be read on its
own: a sentence or two of introduction, what is needed first, the steps,
related guides. A **conceptual guide** defines an idea, shows it with
examples, and links to the guides that put it to use.

A conceptual guide explains the product's ideas as the person using it meets
them. Internal components and data flow are development material, and a "how
it works" page built from the module structure is an architecture document
under another name. A diagram earns its place when it is small enough to
take in at a glance.

In a tree with no naming convention of its own, name a guide for its goal,
verb first (`Deploy Application.md`, `Debug Connection Issues.md`). Link
prerequisites, related concepts and next steps with relative links. Cover
the common ways of working; leave rare cases to the reference.

## Runbooks

A runbook is `docs/runbook.md`, or a `runbook/` directory when there is
enough for one. It holds the procedures for operating a running instance:
deploying, restarting, rotating credentials, recovering from known failures.
Operating your own instance is using the product, so this is user
documentation. Keep it as procedure, with commands copied from manifests, CI
configuration and scripts. Building from source and running locally belong in
the README and guides, not here.

Runbooks are where facts true of only one machine gather. Name the machine
or deployment each procedure applies to, and write a runbook section only
when this session can check it on that machine. An agent that cannot reach
the deployment writes no runbook for it and reports the gap.

Where the project keeps both trees, a runbook page has an agent counterpart
written first, or is declared unpaired (`parallel-sets.md`).
