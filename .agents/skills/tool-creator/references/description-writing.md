# Writing a tool's description

The model chooses a tool from its name, its `description()` and its
parameter schema, and has no other documentation for it. `ToolDefinition`
carries nothing else to the provider: there is no field for example calls.
The description is instruction text, so the swage-prompt-engineering skill
applies to its wording. This file covers what is particular to a tool.

## What the description settles

A reader with the description and no knowledge of the codebase should be
able to answer each of these. Leave out a point the tool gives no reason to
raise.

- **What the tool does**, in its first sentence, as an action on a thing.
- **When it applies, and when a neighbour applies instead.** Where two tools
  could serve the same request, each description names the other and the
  case that belongs to it. A boundary stated on one side only still pulls
  the model the wrong way from the other.
- **What a call changes or costs.** A write to stored state, something the
  person will see, a call to an outside service, money, an effect on another
  session or agent, anything that cannot be undone. The model treats a tool
  as free and reversible unless told otherwise.
- **What comes back**, and what does not: the form of the result, and what
  the tool leaves unreturned or cannot do.
- **What must already be true** for a call to make sense.

Write it as you would explain the tool to someone capable who has never seen
this codebase. Its length is whatever those points take: one sentence for a
tool with no side effects and no neighbour, more for one with either.

## Parameters

Each parameter's `description` in the schema says what the type cannot:

- what the value means, and what choosing it changes;
- its unit, encoding or format;
- for an optional parameter, what happens when it is left out;
- for an `enum`, when each value applies;
- where the value comes from, when it is the output of an earlier call.

A parameter that changes what the tool does as a whole is also named in the
tool's description.

## What the model does with the words

- **A sample value is copied.** A filename, an id or a number written into a
  description comes back in real calls. Describe the property the value must
  have. Where a tool cannot be used without worked example calls, the remedy
  is a field for them on `ToolDefinition`, which is a change to raise, not
  examples pasted into the prose.
- **An internal name is read as guidance.** The model cannot tell a
  codename, a component or a mechanism from an instruction. Describe the tool
  by what it does for its caller (`AGENTS.md`, *Name model-facing surfaces by
  function*), and gloss an identifier that has to appear.
- **Praise carries no information.** A sentence saying how capable or
  efficient the tool is gives the model nothing to decide by.
- **A promise is believed.** A description that claims more than `execute`
  delivers gets the tool called where it fails. State what is true in every
  case the tool is offered in.
