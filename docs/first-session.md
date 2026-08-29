---
description: End-to-end walkthrough of a first session — opening a space, attaching a repository as a workspace, and trusting it so its guidance reaches agents
---

# First session

The [README](../README.md) covers building, starting, and configuring the
gateway. Once it is running with at least one provider configured, the web
UI is where the first session begins.

## Open a space

Opening the web UI presents the space picker — a grid of the spaces this
gateway knows about. A fresh install has one space, `default`, labelled
**Default**. Click a card to preview it (its name, last-opened time,
conversation count, and any attached workspaces appear below the grid),
then click **Open** to enter it.

A space is an isolated working area: it holds its own conversations,
agents, instructions, skills, and workspace attachments. You can create
more spaces from the picker later; for a first session the default space
is enough.

## Attach a workspace

Inside the space, click **Settings** in the top navigation bar. The
settings panel opens with several section tabs; click **Workspaces**.

The workspaces section shows an empty list and a path field. Type the
absolute filesystem path to the repository directory you want this space
to work with — for example, the project you want agents to know about —
and click **Attach workspace**.

The attachment appears in the list, showing the directory path and a trust
indicator: `untrusted · read`. Swage can read files in the workspace but
does not yet trust it to supply guidance.

## Trust the workspace

Newly attached workspaces start untrusted. Until a workspace is trusted,
its `AGENTS.md` and any instruction files at its root are ignored — the
workspace is visible to tools but contributes nothing to the instructions
an agent session receives.

Click **Trust workspace** on the attachment row. The indicator changes to
`trusted · read`, and from this point the workspace's `AGENTS.md` is
folded into agent context for sessions in this space. If the repository
carries project-specific instructions or conventions in that file, agents
will read them alongside the global guidance in `~/.swage/AGENTS.md`.

[Agents and permissions](agents-and-permissions.md#operator-guidance-agentsmd)
explains how workspace guidance layers into the full instruction chain and
what happens when multiple workspaces are attached.

## Revoke trust or detach

Trust is reversible. **Revoke trust** returns the workspace to
`untrusted · read` — its guidance drops out of future turns without
removing the attachment itself. **Detach** removes the workspace from the
space entirely.

## What to do next

With a trusted workspace attached, open a conversation from the space's
main surface and start a turn. The agent will have access to the
workspace's files and will follow whatever guidance its `AGENTS.md`
provides. The [README's sandbox section](../README.md#the-first-turn-may-ask-about-sandboxing)
covers what happens on the first tool call if bubblewrap is not yet
installed.
