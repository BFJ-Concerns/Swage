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

The workspace form takes a filesystem path to an existing repository
directory. The gateway canonicalises whatever it receives, so a relative
path resolves against the gateway process's working directory — use an
absolute path to avoid ambiguity. Enter the path and select **Attach
workspace**.

The attachment appears with its trust state and configured access mode:
`untrusted · read`. The `read` label is the access mode the attachment was
created with — the permissions that will take effect once the workspace is
trusted. While untrusted, the attachment carries no active grants: the
workspace's files are not read, and its `AGENTS.md` is not loaded.

## Trust the workspace

A newly attached workspace is untrusted. An untrusted workspace contributes
nothing — no file access grants and no guidance. Its `AGENTS.md` is not
read into agent context until the workspace is trusted.

Select **Trust workspace** on the attachment. The indicator changes to
`trusted · read`. The configured read permission is now active: the
workspace's `AGENTS.md` feeds the agent context for sessions in this
space.

See [Agents and permissions — Operator guidance](agents-and-permissions.md#operator-guidance-agentsmd)
for how trusted workspace guidance layers into the instruction chain
(global `AGENTS.md` → workspace `AGENTS.md` → agent-definition
instructions).

## Revoke trust and detach

**Revoke trust** returns the workspace to `untrusted · read` — its
read grant and guidance are removed from future turns. **Detach** removes the attachment
entirely.
