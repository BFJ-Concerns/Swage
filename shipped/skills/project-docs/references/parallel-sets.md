# Parallel Documentation Sets

A project past the README-only floor keeps two parallel documentation trees —
one per register — plus generated indexes that guarantee no file goes
unaccounted for. Read this when a project keeps, or this job creates, a `docs/`
tree; a README-only project has no parallel set, and the README serves both
readers.

## Layout

```
README.md          human front door (never mirrored)
llms.txt           generated agent index (llms.txt format)
llms-full.txt      generated agent corpus, one file
docs/              human set: narrative, guides, reference, runbooks
  README.md        generated human index (hand preamble + generated region)
agent-docs/        agent set: the same ground, terse
```

The two trees mirror paths: `docs/reference/cli.md` pairs with
`agent-docs/reference/cli.md`. A page without a counterpart declares itself
`unpaired` in frontmatter (below) — a human-only tutorial, an agent-only
operational detail — so the index check reads declarations, never guesses. The
agent-guidance file (AGENTS.md / CLAUDE.md) sits entirely outside both trees
and keeps its own owner; the boundary is ambient versus on-demand — facts an
agent needs in every session belong to the guidance file, facts it looks up
belong to `agent-docs/`. Whether a project maintains one register, both, or
neither is the project's policy (its planning layer records it); the index
script checks the trees that exist rather than mandating which exist. Both
trees carry usage documentation only (the skill's Scope): a
repository-development fact an agent needs is guidance-file or planning-layer
material, never an `agent-docs/` page, however on-demand it is.

## Precedence: the agent set is primary

Where a fact serves both registers, it is authored in the agent set and the
human page derives from it — restating the same facts with the narrative,
rationale, and worked examples the agent set deliberately strips. Update order
follows: bring the agent page true against the code first, then re-derive the
affected human pages. Human-native material with no agent counterpart —
tutorials, usage rationale (why a reader would choose one option over
another), conceptual guides — is written directly and declared unpaired.

This is the one sanctioned exception to the single-home rule (doctrine gate 3):
the agent page stays the fact's authoritative home, and the human rendering is
a governed derivation, kept honest by the verification chain below — not a
second authority. A derived page earns its file (gate 2) by its rendering:
deleting it would leave the facts machine-terse only, and readability for the
human audience is what the file exists to hold.

## The agent register's shape

An agent page carries what an executing agent acts on and nothing else:
behaviour, commands, configuration, APIs, constraints, gotchas — stated once,
plainly, tables where the content is enumerable. No motivation sections, no
worked walkthroughs, no restated context. The prompt-engineering craft applies
(Two Registers); the evidence and extraction gates apply in full — terseness
never licenses an unverified claim.

## Frontmatter

Both trees carry minimal YAML frontmatter — flat string fields only, and the
one carve-out from the no-frontmatter default:

| Field | Where | Meaning |
|---|---|---|
| `description` | agent pages | One line for `llms.txt` — only when the page title alone is ambiguous; omit otherwise |
| `description` | human pages | One line for the human index — always |
| `unpaired` | either tree | Why this page deliberately has no counterpart |

## Indexes are generated, never hand-edited

`scripts/doc-indexes.sh` derives every index deterministically from the trees
and their frontmatter:

- **`llms.txt`** — the llms.txt format: a hand-written header (H1 title,
  blockquote summary) above the marker, then a generated region of H2-grouped
  link lists, one entry per agent page (`[title](path)`, plus `: description`
  only where frontmatter supplies one), grouped by subdirectory.
- **`llms-full.txt`** — the whole agent set concatenated, fully generated.
- **`docs/README.md`** — the human index: a hand-written preamble above the
  marker, then a generated region listing every human page with its
  frontmatter description, grouped and rendered for a person.

`scripts/doc-indexes.sh --check` regenerates to a temp copy and diffs, and
enforces the mirror rule: every page in either tree has a counterpart at the
mirrored path or an `unpaired` declaration. A stale index, an orphaned page,
or an undeclared one-sided page fails the check — run it in the verify step,
and fix by regenerating or declaring, never by hand-editing a generated
region.

## The verification chain

The accuracy review runs as a chain, priced by where each claim is
authoritative:

1. **Agent set against the code** — the full adversarial audit; this is where
   accuracy is enforced.
2. **Human pages against their agent counterparts** — a derivation check: the
   same facts, no contradictions, nothing invented in the retelling.
3. **Human-only claims against the code** — material with no agent
   counterpart (narrative, tutorials, unpaired pages) cannot be checked
   against the agent set, so it takes the direct audit.

The authority is per claim, not per page, and the writer derives it when
filling the inventory's authority column (`survey-protocol.md`): a claim that
restates its page's agent counterpart carries that counterpart's path; every
other claim — agent pages, unpaired pages, and narrative added inside a
paired page — carries the code. Each leg runs over the claims the job's
scope puts in play (refresh scoping applies); it is not a whole-tree audit
on every touch. The generated artefacts (`llms-full.txt`, the index regions)
are renderings of the trees, not additional fact homes — audit their
generation, never their prose.

## Adopting the scheme in an existing project

A project with only a conventional `docs/` tree is not retrofitted silently:
propose the parallel set as part of the doc plan, and build the agent set from
the fact inventory — it is a write of the terse register, not a mechanical
compression of the human prose. Where the project already keeps its own
doc-site tooling with an agent-export mechanism, conform to the incumbent
system rather than installing this one beside it.
