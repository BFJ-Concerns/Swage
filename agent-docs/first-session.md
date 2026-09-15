---
description: First-session walkthrough — open a topic, optionally attach and trust a workspace, start a conversation with a chosen agent
---

# First session

After the gateway is running and a provider is configured (README), a
first session needs only an opened topic. Attaching a workspace and
trusting it are optional — they give agents file access and project
guidance, but a conversation can start without either.

## Open a topic

The web UI opens to the topic picker. A fresh install has one topic
(`default`, marked **Default**). Select a topic card to preview it, then
select **Open**.

Topics are created from the topic picker; every topic carries its own
conversations, agents, instructions, skills, and workspace attachments.

## Attach a workspace

Inside the opened topic, select **Settings** in the top navigation, then
select **Workspaces** in the settings section navigation.

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

The message input area includes an agent picker that lists the topic's
effective agent definitions — the shipped default (`"gateway"`, display
name **Swage default**) plus any operator-authored definitions visible to
the topic. Selecting an agent and submitting the first message creates a
conversation bound to that definition and navigates to it immediately.

The picker queries `GET /api/topics/{topicId}/agent-definitions`. The
`agent_id` field of `CreateConversationRequest` carries the selection;
omitting it uses the default. See
[Agents and permissions — Effective agent registry](agents-and-permissions.md#effective-agent-registry)
for what the registry contains and how definitions shadow.

## Revoke trust and detach

**Revoke trust** marks the workspace untrusted — its file grants and
guidance are removed from future turns without changing its configured access mode. **Detach** removes the attachment
entirely.
