---
description: First-session walkthrough — start a session with or without a topic, optionally attach and trust a workspace, choose the agent it runs under
---

# First session

After the gateway is running and a provider is configured (README), a
first session needs nothing else. A topic, an attached workspace and trust
in it are optional — they give agents their own memory, file access and
project guidance, but a conversation can start without any of them.

## The front door

The web UI opens to the recent-sessions list (`GET /api/sessions`): every
scope's sessions ordered by their latest user or assistant message, with
automated sessions grouped beneath their source (`entries[].automated`
under a `conversation`, `schedule` or `workflow_run` source). `?topic_id=`
narrows the list to a topic; `?archived=` (boolean) lists the sessions the
inactivity sweep has hidden (`[sessions] archive_after_days`, default 7;
a turn on an archived session unarchives it). Every session records its
origin (`direct`, `schedule_run`, `workflow_worker`, `delegation`).
Selecting an entry resumes it (`/conversations/{id}`, or
`/conversations/{id}/surfaces/{surface_id}` with a surface open).

Beneath the list, the **New session** control takes a topic (or none) and
an agent, then **Start** opens the composer in that scope. Nothing is
created until the first message: the composer's send creates the
conversation (`POST /api/topics/{topic_id}/sessions`, or
`POST /api/conversations` for the general scope) and then sends. A start
abandoned before the first message leaves no session behind. **+ New
topic** creates a topic (`POST /api/topics`) without starting a session.

## Topics

A topic is optional shared context — memory, knowledge base, attached
workspaces, configuration — for the sessions started in it. A session
started without one runs in the general scope: general memory, the general
knowledge base, no inherited files or context from any topic, and no
`topic_id` on the wire. `GET /api/topics` never lists the general scope,
and it cannot be opened or deleted. Schedules can run in the general scope:
a schedule created without a topic resolves to the general scope.

A session without a topic sees a built-in orientation element at every
turn, describing its environment: no topic is selected, general memory and
the general knowledge base are in scope, and tool paths resolve at a
scratch directory for the session
(`scratch/general/sessions/<session_id>` under the data root). A session
in a topic whose workspace is attached sees its working directory instead;
the scratch layout for topic sessions is
`scratch/topics/<topic>/sessions/<session_id>`.

The top bar names the session's scope; a topic's name links to its settings
page (`/settings/topic/{slug}/{overview|workspaces|surfaces|sessions}`),
where its description, workspaces, default surfaces, tags and sessions are
managed. Every topic is also listed in the settings section navigation.

## Attach a workspace

Open the topic's settings page and select its **Workspaces** tab.

The workspace form takes a filesystem path to an existing repository
directory. The gateway canonicalises whatever it receives, so a relative
path resolves against the gateway process's working directory — use an
absolute path to avoid ambiguity. **Allow file changes** defaults on;
**Trust this workspace** defaults off. Select the access and trust choices,
then **Attach workspace**. The first attachment becomes the default working
folder once trusted, for conversations without an explicit workspace selection.

The attachment appears with its trust state and configured access mode:
read and write access by default, with trust disabled unless selected.
The access mode describes the permissions that take effect once trusted.
While untrusted, the attachment grants no file access and does not
load its `AGENTS.md`.

## Trust the workspace

Unless trusted during attachment, a newly attached workspace is untrusted.
An untrusted workspace contributes nothing — no file access grants and no guidance. Its `AGENTS.md` is not
read into agent context until the workspace is trusted.

Select **Trust workspace** on an untrusted attachment. Its configured file
permissions become active, and the workspace's `AGENTS.md` feeds the agent context for sessions in this
topic.

See [Agents and permissions — Operator guidance](agents-and-permissions.md#operator-guidance-agentsmd)
for how trusted workspace guidance layers into the instruction chain
(global `AGENTS.md` → workspace `AGENTS.md` → agent-definition
instructions).

## Start a conversation with a chosen agent

The agent picker — on the front door's **New session** control, and in the
composer until the first message — lists the scope's effective agent
definitions: the shipped default (`"gateway"`, display name **Swage
default**) plus any operator-authored definitions visible to the topic, or
the global ones for the general scope. Selecting an agent and submitting the
first message creates a conversation bound to that definition and navigates
to it immediately.

The picker queries `GET /api/topics/{topicId}/agent-definitions`, or
`GET /api/agent-definitions` for the general scope. The `agent_id` field of
`CreateConversationRequest` carries the selection; omitting it uses the
default. Every conversation read (`ConversationResponse`,
`ConversationSummary`, `ConversationDetail`) carries the `agent_id` the
session runs under. See
[Agents and permissions — Effective agent registry](agents-and-permissions.md#effective-agent-registry)
for what the registry contains and how definitions shadow.

## Revoke trust and detach

**Revoke trust** marks the workspace untrusted — its file grants and
guidance are removed from future turns without changing its configured access mode. **Detach** removes the attachment
entirely.
