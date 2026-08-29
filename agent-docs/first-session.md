---
description: First-session walkthrough — open a space, attach a workspace, trust it so its AGENTS.md feeds agent context
---

# First session

After the gateway is running and a provider is configured (README), the
first session requires a space, a workspace attachment, and a trust
decision.

## Open a space

The web UI opens to the space picker. A fresh install has one space
(`default`, marked **Default**). Select a space card to preview it, then
select **Open**.

Spaces are created from the space picker; every space carries its own
conversations, agents, instructions, skills, and workspace attachments.

## Attach a workspace

Inside the opened space, select **Settings** in the top navigation, then
select **Workspaces** in the settings section navigation.

The workspace form takes an absolute filesystem path to a repository
directory. Enter the path and select **Attach workspace**.

The attachment appears with its trust state: `untrusted · read`.

## Trust the workspace

A newly attached workspace is untrusted. An untrusted workspace's
`AGENTS.md` and instructions are not read into agent context — the
workspace contributes nothing until trusted.

Select **Trust workspace** on the attachment. The trust state changes to
`trusted · read`. The workspace's `AGENTS.md` now feeds the agent context
for sessions in this space.

See [Agents and permissions — Operator guidance](agents-and-permissions.md#operator-guidance-agentsmd)
for how trusted workspace guidance layers into the instruction chain
(global `AGENTS.md` → workspace `AGENTS.md` → agent-definition
instructions).

## Revoke trust and detach

**Revoke trust** returns the workspace to `untrusted · read` — its
guidance is removed from future turns. **Detach** removes the attachment
entirely.
