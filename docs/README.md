# Documentation

These pages describe Swage as it is built today. The [project
README](../README.md) covers installing, running and configuring a gateway;
start there if you have not run one yet. The pages below pick up where it
stops, and go into the parts of the system whose behaviour is hard to infer
from the outside — what a workflow run does when the gateway restarts under
it, what a tool call is permitted to touch, what the web UI can show you
about a turn after the fact.

Start with [provider setup](interfaces-and-tooling.md#provider-setup), then
[first session](first-session.md). The remaining pages cover operating the
platform and inspecting its behaviour.

<!-- doc-index:start (generated region; do not hand-edit. Regenerate with the project-docs skill: doc-indexes.sh) -->
## General

- **[Agents and permissions](agents-and-permissions.md)** — How Swage defines agents (including conversational authoring and the API), reads operator guidance from AGENTS.md, resolves tool permissions, enforces approval, isolates tool execution, handles delegated grant inheritance, resolves effort per model, configures context assembly and compaction, controls experimentation and variant selection, handles wall-triggered succession, and configures voice transcription and synthesis
- **[First session](first-session.md)** — End-to-end walkthrough of a first session — starting a session with or without a topic, optionally attaching and trusting a workspace, and choosing the agent it runs under
- **[Interfaces and tooling](interfaces-and-tooling.md)** — Provider setup and credentials, protocol selection, network access, Settings, turn inspection, conversations, surfaces, devices, gateway lifecycle, API reference, voice and tools
- **[Workflow engine](workflow-engine.md)** — How Swage runs workflow scripts — lifecycle, journalling, budget management (including per-route), run detail, terminal account (how a run ended, worker standings, partial results), durable parking, and failure handling
<!-- doc-index:end -->
