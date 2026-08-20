# Document Shapes

Shapes and formats for files that have earned existence (doctrine gate 2). These
are what a justified file looks like, not licences to create one — a section with
nothing grounded to say is omitted, not stubbed.

The shapes map the classic documentation purposes: learning (tutorials),
solving a task (how-to guides), looking things up (reference), and
understanding (conceptual guides). Classify by the reader's purpose before
picking a shape. All of them document using the product; how it is developed
is out of scope entirely (the skill's Scope), so there is no architecture,
decision-record, or development-doc shape here.

## Reference

Home `docs/reference/`, one file per surface, created as needed — not every
project requires any of these:

| File | Content |
|------|---------|
| `cli.md` | Commands, subcommands, flags, arguments |
| `configuration.md` | Config options, types, defaults, valid values |
| `api.md` | Endpoints, methods, parameters, responses, errors |
| `errors.md` | Error codes, meanings, causes, resolutions |
| `glossary.md` | Domain terms, acronyms, jargon |

Where the project auto-generates reference docs (OpenAPI, TypeDoc, rustdoc and
kin), the generated output is the source of truth: summarise it, link to it, and
supplement it with the human context generators miss — the *why* and *when* of
using an endpoint or option. Do not duplicate it by hand.

Table formats:

```markdown
| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--verbose` | `-v` | `false` | Enable detailed output |
```

```markdown
| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `timeout` | integer | `30` | Request timeout in seconds |
```

```markdown
| Parameter | Required | Type | Description |
|-----------|----------|------|-------------|
| `user_id` | Yes | string | Unique user identifier |
```

```markdown
| Code | Name | Description | Resolution |
|------|------|-------------|------------|
| `E001` | InvalidToken | Authentication token expired | Re-authenticate |
```

Guidelines: be exhaustive — every option, extracted per doctrine gate 5; show
defaults and types; wrap values and parameter names in backticks; group large
tables with subheadings.

## Guides

Home `docs/guides/`. Two kinds, distinguished naturally by their titles:

| Tutorials | How-to guides |
|-----------|---------------|
| "Learn to build a plugin" | "Add authentication to a plugin" |
| Linear, start to finish | Readable in isolation |
| For newcomers | For practitioners |
| Teach concepts | Assume concepts |

Name guides `Verb Goal.md` — `Deploy Application.md`, `Debug Connection
Issues.md` — and open each with an H1 title.

- **Tutorial structure**: what the reader will learn; prerequisites (linked);
  linear steps under clear headings; what was learned; next steps.
- **How-to structure**: one- or two-sentence introduction; prerequisites
  (linked); goal-focused steps; related guides at the end.
- **Conceptual guide**: brief definition; key points with examples; links to the
  practical guides that apply the concept. It explains the product's concepts
  as its reader uses them — never internal components or data flow, which are
  development material (the skill's Scope); a "How It Works" page rebuilt from
  module structure is the removed architecture doc under another name. A
  Mermaid diagram, where one genuinely helps, keeps to 5–7 boxes — an
  overloaded diagram communicates less than a small one.

Link with relative Markdown links: prerequisites, related concepts, next steps,
and worked examples. A well-linked guide carries roughly 3–10 internal links
depending on length. Keep guides practical — the common workflows, not the edge
cases.

Highlight genuine tips and cautions with GFM alerts where the forge renders
them — `> [!TIP]`, `> [!WARNING]`, `> [!NOTE]` — and keep them rare enough to
stay meaningful. Long optional material (full command output, an extended
example) can sit in a collapsible `<details>` block with a clear `<summary>`.

## Runbooks

Home `docs/runbook.md` (or a `runbook/` directory when earned): operational
procedure for a deployed instance — deploying, restarting services, rotating
credentials, recovering from known failures. Operating one's own instance is a
user activity, so this is usage documentation; keep it procedural, with the
commands extracted from manifests, CI, and scripts (doctrine gate 5) rather
than recalled. Building from source and running locally are usage material
too, but they live in the README's install/build lines and, when a project
earns one, a how-to guide — not here. Under parallel sets a runbook page
follows the normal mirror rule: author its agent counterpart first, or
declare the page unpaired (`parallel-sets.md`).

Runbook content is where machine-conditioned facts concentrate, and doctrine
gate 6 applies in full: name the machine or deployment each procedure is true
on, and propose a runbook section only when this session can verify its claims
on that machine — an agent that cannot reach the deployment writes no runbook
for it, and reports the gap instead. Gate 6 governs truth-scoping; the privacy
rule (`survey-protocol.md`) still decides what may be published at all.
