---
description: End-to-end walkthrough of a first session — opening a space, optionally attaching and trusting a workspace, and starting a conversation with a chosen agent
---

# First session

The [README](../README.md) covers building, starting, and configuring the
gateway. Once it is running with at least one provider configured, the web
UI is where the first session begins. A first session needs only an opened
space — attaching a workspace and trusting it are optional steps that give
agents file access and project guidance, but a conversation can start
without either.

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
path to the repository directory you want this space to work with — for
example, the project you want agents to know about. The gateway
canonicalises the path, so a relative path resolves against the gateway
process's working directory; an absolute path is the safer choice. Click
**Attach workspace**.

The attachment appears in the list, showing the directory path and a trust
indicator: `untrusted · read`. The `read` label is the access mode
configured on the attachment — the permission that activates once the
workspace is trusted. While untrusted, the attachment carries no active
grants: the workspace's files are not read, and its `AGENTS.md` is not
loaded.

## Trust the workspace

Newly attached workspaces start untrusted. An untrusted workspace
contributes nothing — no file access grants and no guidance. Its
`AGENTS.md` is not read into agent context until the workspace is trusted.

Click **Trust workspace** on the attachment row. The indicator changes to
`trusted · read`, and from this point the configured read permission is
active: the workspace's `AGENTS.md` is folded into agent context for
sessions in this space. If the repository
carries project-specific instructions or conventions in that file, agents
will read them alongside the global guidance in `~/.swage/AGENTS.md`.

[Agents and permissions](agents-and-permissions.md#operator-guidance-agentsmd)
explains how workspace guidance layers into the full instruction chain and
what happens when multiple workspaces are attached.

## Revoke trust or detach

Trust is reversible. **Revoke trust** returns the workspace to
`untrusted · read` — its read grant and guidance drop out of future turns
without removing the attachment itself. **Detach** removes the workspace from the
space entirely.

## Start a conversation

The message input area at the bottom of the space includes an agent picker
alongside the text field. The picker is available whenever there is no
active conversation in the opened space. It lists the space's effective
agent definitions — the shipped default (**Swage default**) plus any
operator-authored definitions visible to the space. Select an agent, type a
message, and submit: the gateway creates the conversation bound to that
agent definition and navigates to it immediately.

If only the shipped default is available (a fresh install with no
operator-authored definitions), the picker has one entry and the choice is
automatic.

[Agents and permissions](agents-and-permissions.md#what-agents-are-available)
covers what the registry contains and how space-scoped definitions shadow
global ones.

## What to do next

With a conversation open, start a turn. If a trusted workspace is
attached, the agent will have access to the workspace's files and will
follow whatever guidance its `AGENTS.md` provides.
The [README's sandbox section](../README.md#the-first-turn-may-ask-about-sandboxing)
covers what happens on the first tool call if bubblewrap is not yet
installed.
