# Survey Protocol

How to ground documentation in the code: what counts as evidence, where to look,
and what the working fact inventory records. The survey produces evidence for the
writer; it produces no committed artefact.

## Surface Discovery

Before reading deeply, enumerate what the project exposes. Typical places:

- **Existing docs** — `README*`, `docs/**`, any stray `*.md`, existing
  agent-guidance files (`AGENTS.md`, `CLAUDE.md` and pointers).
- **Manifests** — `Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod`,
  `Makefile`, `justfile`: names, entry points, scripts, dependency intent.
- **CI configs** — workflow files reveal the real build and test commands.
- **Interfaces** — arg parsers and CLI definitions; config structs, schemas, and
  env handling; route definitions or OpenAPI specs; error enums and constants.
- **Examples and tests** — working usage the prose can be checked against.

## Evidence Classes

Each claim class has sources that can ground it:

| Claim about | Grounded by |
|---|---|
| Behaviour | The implementation, and tests that exercise it |
| Commands (build/test/run) | Manifests, CI configs, scripts actually present |
| Configuration | The config schema, parser, defaults, env handling |
| CLI surface | The arg parser; `--help` output from a safe run |
| API surface | Route definitions, OpenAPI specs, controllers |
| Errors | Error enums, constants, handling paths |

Not evidence on their own: TODOs, roadmap comments, dead code,
feature-flagged-off paths, and existing docs (they are the thing under audit).

When sources conflict, precedence is: code > tests > CI configs > examples >
existing docs. A conflict worth resolving in the docs is usually also worth
mentioning in the report.

## The Fact Inventory

Working material, kept in the session's scratch space and never committed. One
row per substantive claim:

```
claim | source (file:line or command run) | audience | canonical home | authority | uncertainty
```

The authority column names what the accuracy review checks the claim against:
`code`, or the mirrored agent counterpart's path for a claim that restates one
(`parallel-sets.md`, The verification chain). The writer fills it when
assembling the inventory — survey workers do not report it.

Audience is who acts on the fact — executing agent, orienting user, deep-diving
user, operating user (running a deployed instance; their facts route to the
runbook) — and drives the canonical-home choice. A fact true only on a
particular machine or deployment records that condition in its row (doctrine
gate 6), so the writer scopes it rather than generalising it. Record uncertainty
honestly; an unverified row is a row the accuracy review must resolve or the
docs must omit.

Delegated survey workers return rows in this shape (see
`../prompts/survey-worker.md`); they never return prose for pasting.

## Safe-Command Policy

Documented commands are worth sanity-checking, within limits: read-only
invocations (`--help`, `--version`, list subcommands) are fine by default;
builds, test suites, and anything that mutates state run only when they are
already part of the session's work or the user asks. Never run a command just to
document it if its effects are unclear.

## Generated Content

Documentation produced by a generator (API references, coverage tables, managed
sections between markers) is never hand-edited: update the generator's input, or
report the blocker. Hand edits are overwritten on the next generation and hide
the real fix.

## Privacy

Config files, env handling, and CI secrets sections get read during a survey.
Never publish secrets, credentials, tokens, private hostnames, or machine-local
paths into documentation — describe the shape of the configuration ("an API key
in `API_KEY`") without the values.

## Monorepos

Survey per package, and document at the level readers land: the root README
orients across packages; a package that is independently consumed carries its own
README and reference detail. One agent-guidance hand-off at repo level unless
packages already keep their own guidance files.
