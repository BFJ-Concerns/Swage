# Documentation

These pages describe Swage as it is built today. The [project
README](../README.md) covers installing, running and configuring a gateway;
start there if you have not run one yet. The pages below pick up where it
stops, and go into the parts of the system whose behaviour is hard to infer
from the outside — what a workflow run does when the gateway restarts under
it, what a tool call is permitted to touch, what the web UI can show you
about a turn after the fact.

Read them in any order. Each is self-contained, and they overlap only where
one subject genuinely reaches into another: sandbox isolation belongs to
permissions, and the verification gate belongs to tooling, but both surface
in the README's setup instructions too.

[architecture-flow.svg](architecture-flow.svg) draws the whole system on one
page — a turn's route from a client, through the gateway, to a provider and
back — and is the fastest way to place any of these subjects in context.

<!-- doc-index:start (generated region; do not hand-edit. Regenerate with the project-docs skill: doc-indexes.sh) -->
## General

- **[Agents and permissions](agents-and-permissions.md)** — How Swage defines agents, reads operator guidance from AGENTS.md, resolves tool permissions, enforces approval, isolates tool execution, handles delegated grant inheritance, and resolves effort per model
- **[First session](first-session.md)** — End-to-end walkthrough of a first session — opening a space, attaching a repository as a workspace, and trusting it so its guidance reaches agents
- **[Interfaces and tooling](interfaces-and-tooling.md)** — Network binding, the web UI's context inspector, image attachments, the API error contract, wire-contract architecture with the route manifest and its projections, verification gate, and route handling
- **[Workflow engine](workflow-engine.md)** — How Swage runs workflow scripts — lifecycle, journalling, budget management, durable parking, and failure handling
<!-- doc-index:end -->
