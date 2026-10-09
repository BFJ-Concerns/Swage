# Agent Notes

## Status

Private alpha — internal dogfooding, no external users. Don't build
backward-compatibility or data-migration paths for schema and format
changes; change them freely and let the fresh migration rebuild. Strip out any existing migrations when working in the area. This is a
deliberate "for now": it lifts the day there are users with data to migrate.

## Build and Verification

Use the repository's `just` targets rather than assembling equivalent command
sequences by hand:

- `just check` for the narrow workspace/frontend type check only.
- `just check-gate` for the fast, pre-test static gate: tracked-local-state,
  formatting, workspace/frontend checking, frontend design-token checking, and
  clippy.
- `just build-dev-embedded` when the gateway needs the current frontend embedded.
- `just test` for the full Rust, frontend, and desktop-smoke suite.
- `just verify` before landing (`just ci` is an alias). It runs `check-gate`,
  the test suite, and a final check that the committed protocol projections
  are not stale. CI runs this same recipe, so a green run here means CI's
  checks passed — a new check belongs in `xtask`, not in the workflow file.

To see a change working in the running app, start it with `SWAGE_HOME`
pointing at a fresh directory, so the run never opens or migrates your own
`~/.swage` data, then drive the web UI in a browser or the desktop shell
directly. Keep driving scripts, screenshots and traces outside the repository
rather than adding a project-local driver.

## Working Principles

### Prefer the general path over the special case

Unless it is genuinely unavoidable, don't add a special case where a general
one would serve. When a requirement could be met either by a one-off branch or
by extending a shared path that future work can reuse, extend the shared path.
Special cases multiply — each becomes the template for the next, and the
general capability that would have absorbed all of them never gets built. If a
special case truly is unavoidable, say why at the call site.

### Name model-facing surfaces by function

Anything the model reads should present itself by what it *is* or *does*, not
by the component, codename, or mechanism behind it. This covers labels and
headings (a context section title), the values inside them (a status, a
category), and the framing around them (a tool description, the opening line of
an agent prompt). The model can't distinguish an internal label from genuine
guidance: `## Conversation Summary` says what the block is;
`## Conversation Summary (Structured Extraction)` leaks the mechanism that
produced it and tells the model nothing useful.

Keep internal identifiers — agent ids, struct names, context element keys like
`"prompts"` and `"history"` — in the code, where they address developers. Where
the same concept crosses into the model's context, describe it by function. The
built-in tool descriptions get this right (`file_read` opens "Read the contents
of a file in the workspace…"); the assembly element keys stay inside the report
and never reach the model.

### Build for experimentation and audit

A core purpose of Swage is to A/B test and record how it works — which
prompt, which tool implementation, which setting produces the better
result. Build for that from the start: when you add or change anything the
system does, ask how it will be recorded and how it could be varied.

Two consequences for new work:

- **Record to one unified, local audit trail.** Whatever a change does — a
  turn, a tool call, an assembly decision, the selection of a variant —
  should be reconstructable afterwards from a single local record, not
  scattered across ad-hoc logs or lost. Everything stays on the machine.
- **Express the choice as a swappable variant, hidden from user and
  model.** Where a behaviour could be done more than one way — a prompt for
  a task, an entire alternative tool implementation, a tunable setting —
  model the alternatives as variants selected per session or per task
  behind a stable surface. The user and the model see the function; neither
  sees which variant is running. This is "Name model-facing surfaces by
  function" applied to experiments: the variant is the mechanism, and the
  mechanism stays hidden.

Experimentation must be switchable off. There has to be a backend plug to
disable it globally and per location — down to a single important task that
shouldn't become anyone's sample. The frontend control isn't built yet; the
backend capability is the requirement, so leave the seam for it even though
nothing toggles it from the UI today.

### The wire contract is one definition, projected

Anything that crosses a process boundary — HTTP request/response bodies,
WebSocket events, device commands — belongs in the `swage-protocol` crate as
plain serde types, defined once. Every consumer is a derived projection: Rust
clients depend on the crate directly; the frontend's TypeScript is generated
from it (ts-rs); the canonical published artifact is the JSON Schema
(schemars), committed to the repo and freshness-checked in CI. If a projection
and the schema ever disagree, the schema wins.

The crate is organised by surface, each emitting its own schema document:
client (conversation, streaming, approvals), hosting/operator, device (duplex
and server-initiated — the server invokes operations on remote execution
clients over a connection the device dialled in, with correlation IDs,
capability advertisement, cancellation, and heartbeats as first-class fields),
and channel (the contract external integrators speak). Subset clients
implement only their surfaces; external parties see only the surface
published to them.

The migration is complete. Do not hand-author wire-contract copies in the
gateway, clients, or frontend. Frontend view types may adapt imports from
`frontend/src/lib/protocol/`, but the wire definition remains in
`swage-protocol` and its generated projections.
