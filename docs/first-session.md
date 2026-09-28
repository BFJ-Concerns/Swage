---
description: End-to-end walkthrough of a first session — starting a session with or without a topic, optionally attaching and trusting a workspace, and choosing the agent it runs under
---

# First session

The [README](../README.md) covers building, starting, and configuring the
gateway. Once it is running with at least one provider configured, the web
UI is where the first session begins. A first session needs nothing else —
a topic, a workspace attached to it, and trust in that workspace are
optional steps that give agents their own memory, file access and project
guidance, but a conversation can start without any of them.

## The front door

Opening the web UI presents the recent-sessions list — every session across
every topic and the general scope, newest message first. Each entry's
preview shows the latest user or assistant message; tool and system messages
never replace the preview. **All sessions** is the default view; the topic
buttons narrow the list to one topic without changing anything else, and
**Archived** (the `archived` boolean query parameter on the sessions route)
shows the sessions the inactivity sweep has hidden. Automated sessions (a
schedule's runs, a workflow's workers, delegated children) sit beneath the
conversation or schedule that caused them, each still its own session.
Select any entry to resume it with its conversation and surfaces; **All
sessions** in the top bar returns here.

Beneath the list is the **New session** control. Choose a topic from its
selector, or leave it at **No topic — general assistant**; choose the agent
the session should run under (the operator's default is preselected); then
click **Start**. The composer opens in that scope, and the first message
creates the conversation there. Nothing is created until then, so a start
you walk away from leaves no empty session behind. **+ New topic** beside
the selector creates a topic without starting anything.

Sessions nobody has spoken in for seven days leave the recent list. The
delay is set under **Settings → Automation → Session archiving**. Only a
session with a turn executing stays regardless of age; a new turn on an
archived session brings it back, and archiving cancels nothing and blocks no
scheduled continuation.

## Topics

A topic is optional shared context: its own memory and knowledge base, its
attached workspaces, and its configuration, applied to every session started
in it. A session started without one is a fully usable general assistant in
the general scope — general memory, the general knowledge base, the ordinary
granted tools — and inherits nothing from any topic, least of all the last
one used. The general scope is not a topic: it is never listed, and cannot
be opened or deleted. Schedules can run in the general scope: a schedule
created without a topic resolves to the general scope.

A session without a topic receives a built-in orientation at every turn,
describing its environment: no topic is selected, general memory and the
general knowledge base are in scope, and tool paths resolve at a scratch
directory for the session. A session in a topic with an attached workspace
sees its working directory instead. The scratch layout under the data root
is `scratch/general/sessions/<session_id>` for topicless sessions and
`scratch/topics/<topic>/sessions/<session_id>` for topic sessions.

The top bar names the session's topic (or **General**). Clicking a topic's
name opens its settings page, where its description, workspaces, default
surfaces, tags and sessions are managed; every topic is also listed in the
**Settings** sections.

## Attach a workspace

Open the topic's settings — click its name in the top bar, or choose it
under **Settings** — and click the **Workspaces** tab.

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

The agent picker appears on the front door's **New session** control and,
once a session has started, in the composer's toolbar until the first
message is sent. It lists the scope's effective agent definitions — the
shipped default (**Swage default**) plus any operator-authored definitions
visible to the topic, or the global ones for a session without a topic.
Select an agent, type a message, and submit: the gateway creates the
conversation bound to that agent definition and navigates to it immediately.
The agent a session runs under is part of every conversation read, so the
governing definition is visible before the first message.

If only the shipped default is available (a fresh install with no
operator-authored definitions), the picker has one entry and the choice is
automatic.

**+ New** in the conversation list or the channel's session bar starts
another session in the same scope with the agent currently picked.

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
