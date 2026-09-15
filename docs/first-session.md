---
description: End-to-end walkthrough of a first session — opening a topic, optionally attaching and trusting a workspace, and starting a conversation with a chosen agent
---

# First session

The [README](../README.md) covers building, starting, and configuring the
gateway. Once it is running with at least one provider configured, the web
UI is where the first session begins. A first session needs only an opened
topic — attaching a workspace and trusting it are optional steps that give
agents file access and project guidance, but a conversation can start
without either.

## Open a topic

Opening the web UI presents the recent-sessions list — every session across
every topic and the general scope, newest message first — with the topic
cards beneath it. **All sessions** is the default view; the topic buttons
narrow the list to one topic without opening it, and **Archived** shows the
sessions the inactivity sweep has hidden. Automated sessions (a schedule's
runs, a workflow's workers, delegated children) sit beneath the conversation
or schedule that caused them, each still its own session. Select any entry
to resume it; **All sessions** in the top bar returns here.

A fresh install has no topics: a conversation started without one runs as a
general assistant in the general scope, with general memory and the general
knowledge base, and inherits nothing from any topic. Create a topic from the
cards when work should keep its own memory, attached workspaces and
configuration; click a card to preview it (its name, last-opened time,
conversation count, and any attached workspaces appear below the grid), then
click **Open** to enter it.

Sessions nobody has spoken in for seven days leave the recent list. The
delay is set under **Settings → Automation → Session archiving**. Only a
session with a turn executing stays regardless of age; a new turn on an
archived session brings it back, and archiving cancels nothing and blocks no
scheduled continuation.

A topic is an isolated working area: it holds its own conversations,
agents, instructions, skills, and workspace attachments. The general scope
is not a topic — it is never listed, and cannot be opened or deleted.

## Attach a workspace

Inside the topic, click **Settings** in the top navigation bar. The
settings panel opens with several section tabs; click **Workspaces**.

The workspaces section shows an empty list and a path field. Type the
path to the repository directory you want this topic to work with — for
example, the project you want agents to know about. The gateway
canonicalises the path, so a relative path resolves against the gateway
process's working directory; an absolute path is the safer choice.

**Allow file changes** is selected by default. Leave it selected for coding,
or clear it for a repository the agent should only read. Select **Trust this
workspace** if you want to grant that access immediately, then click
**Attach workspace**. Once trusted, the first attachment becomes the working
folder for conversations without an explicit workspace selection.

The attachment appears in the list, showing the directory path and a trust
indicator and configured read/write access. Unless you selected trust
during attachment, the workspace is untrusted; its configured permissions
activate only when it is trusted. An untrusted attachment grants no file
access and does not load its `AGENTS.md`.

## Trust the workspace

Workspaces start untrusted unless you choose trust during attachment.
An untrusted workspace contributes nothing — no file access grants and no guidance. Its
`AGENTS.md` is not read into agent context until the workspace is trusted.

Click **Trust workspace** on an untrusted attachment row. Its configured
file permissions become active: the workspace's `AGENTS.md` is folded into
agent context for sessions in this topic. If the repository
carries project-specific instructions or conventions in that file, agents
will read them alongside the global guidance in `~/.swage/AGENTS.md`.

[Agents and permissions](agents-and-permissions.md#operator-guidance-agentsmd)
explains how workspace guidance layers into the full instruction chain and
what happens when multiple workspaces are attached.

## Revoke trust or detach

Trust is reversible. **Revoke trust** marks the workspace untrusted — its
file grants and guidance drop out of future turns
without removing the attachment itself. **Detach** removes the workspace from the
topic entirely.

## Start a conversation

The message input area at the bottom of the topic includes an agent picker
alongside the text field. The picker is available whenever there is no
active conversation in the opened topic. It lists the topic's effective
agent definitions — the shipped default (**Swage default**) plus any
operator-authored definitions visible to the topic. Select an agent, type a
message, and submit: the gateway creates the conversation bound to that
agent definition and navigates to it immediately.

If only the shipped default is available (a fresh install with no
operator-authored definitions), the picker has one entry and the choice is
automatic.

[Agents and permissions](agents-and-permissions.md#what-agents-are-available)
covers what the registry contains and how topic-scoped definitions shadow
global ones.

## What to do next

With a conversation open, start a turn. If a trusted workspace is
attached, the agent will have access to the workspace's files and will
follow whatever guidance its `AGENTS.md` provides.
The [README's sandbox section](../README.md#the-first-turn-may-ask-about-sandboxing)
covers what happens on the first tool call if bubblewrap is not yet
installed.
