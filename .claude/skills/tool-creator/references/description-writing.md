# Description Writing

How to write the `description()` string and per-parameter descriptions so the model picks the right tool and calls it correctly. Read this whenever the work is on the description text itself, regardless of whether the surrounding tool design is changing.

Anthropic's own guidance treats this as the highest-leverage surface in tool design — refining descriptions alone produced state-of-the-art SWE-bench results without changing implementations. Treat it accordingly.

This file gives criteria to write against, not sample descriptions to copy. Generate each description from the tool in front of you; a borrowed example anchors the model to values that do not fit and ages badly as tools change.

## The Framing

Write the description the way you would describe the tool to a new hire on their first day.

- Make implicit context explicit — niche terminology, prerequisite state, the relationship between this tool and the ones around it.
- Assume the reader does not have tribal knowledge of the codebase. The model has none.
- Skip nothing on the grounds of "obvious to anyone familiar with this area."

The new-hire framing is the single most useful prompt for catching what is missing. When stuck, ask: would a new hire know this from the description alone?

## Length

Aim for **at least 3-4 sentences**, more for complex tools or tools with subtle triggers. The short-and-snappy instinct is wrong for tool descriptions consumed by the model.

The exception is a genuinely trivial tool with no side effects and no near-lookalikes — a single sentence is fine when the name, the schema, and the implementation leave no real ambiguity. Most tools are not in this category.

Length is not the goal; coverage is. If a short description covers every item in the checklist below, fine. If it is short because something is missing, add the missing thing rather than trimming further.

## What The Description Must Cover

Every `description()` string should answer all of:

1. **What the tool does.** The function, named as a verb on a resource. First sentence.
2. **When the model should call it.** The trigger condition — the user state, session state, or point in the conversation that warrants the call.
3. **When the model should *not* call it.** The anti-trigger — nearby lookalikes, related-but-different operations, or situations where the model is tempted but should use something else. This is the single most common omission.
4. **What each parameter means and how it affects behaviour.** Not the type — the schema already says that. The semantic meaning and the consequence of getting it wrong. Most parameter detail lives in the per-parameter `description:` fields, but if a parameter changes the tool's behaviour categorically, name that in the tool description too.
5. **Important caveats and limitations.** What the tool does not return, what it cannot do, what state it requires, what it ignores.
6. **Side effects.** Whether the call writes state, emits to the user, hits an external service, costs money, or is hard to reverse. The model otherwise treats every tool as equally cheap and reversible.

## Side Effect Documentation

Side effects deserve their own attention because the model has no other way to learn them. Be explicit about:

- **Writes to persistent state.** Database rows, files, configuration changes.
- **User-visible output.** Anything that lands in the user's UI, chat, or notifications.
- **External calls.** API hits, paid services, rate-limited resources.
- **Irreversibility.** Deletions, finalisations, anything that cannot be undone by a subsequent call.
- **Cross-session or cross-agent effects.** Anything that affects sessions or agents other than the one calling the tool.

A tool that writes to persistent storage, emits something to the user, and triggers a downstream check is doing three visible things. The description must name all three.

## Anti-Trigger Disambiguation

When two or more tools could plausibly match the same situation, every description in the cluster must name the others and say which one wins for which case. The model selects from descriptions alone — if the descriptions do not distinguish the tools, the model cannot either.

The shape is always the same: state what this tool is for, then name the sibling and the case that belongs to it instead. Typical splits the model needs spelled out are selecting by identifier versus searching by criteria, creating a new record versus amending an existing one, and reading versus writing the same resource.

Put the anti-trigger in *every* description in the cluster, not just the new one. A one-sided anti-trigger still pulls the model toward the wrong choice from the other side.

## Per-Parameter Descriptions

Each parameter in the JSON Schema needs its own `description:` field. The same principles apply, scaled down.

- **Describe meaning, not type.** The schema gives the type; the description says what kind of value of that type is wanted.
- **State units and formats.** Whatever constrains the value — its unit, its encoding, its format — must be stated, since the type alone does not carry it.
- **Name the consequence of getting it wrong.** Telling the model how a value changes behaviour tells it how to choose. Naming the field without its effect does not.
- **For enums, repeat the values inline and explain each.** The schema's `enum` carries the values; the description explains when each one applies.
- **For optional parameters, name the default behaviour.** Stating what happens on omission tells the model when omitting is the right choice.
- **Identify where the value comes from.** Pointing at the source of a value — the response field, index, or prior call it should come from — closes the loop between tools.

## Anti-Patterns

These are the recurring failure modes.

- **Restating the name.** A description that only re-expresses the tool's name in a sentence wastes the slot. The name already says that; use the description for what the name cannot convey.
- **Marketing voice.** Praise of how powerful or efficient the tool is tells the model nothing it can act on. Write operational documentation, not a product pitch.
- **Opaque jargon without context.** Internal identifiers and encodings are weaker than plain terms for the things the model has to reason about. Where internal jargon is unavoidable in an identifier, gloss it in the description.
- **Concrete worked examples in description prose.** The model reproduces example values it is shown, so a sample input baked into the description leaks into real calls. `ToolDefinition` forwards only `name`, `description`, and `input_schema` to the provider — there is no `input_examples` channel — so if a tool genuinely needs worked example inputs, the fix is to add that channel to the definition, not to paste examples into the description (see `tool-design.md`).
- **Burying the trigger.** The first sentence should answer "what does this do?". Front-load the function; caveats and edge cases come after.
- **Treating the description as the place for implementation detail.** Authentication mechanics, pagination policy, rate limits — those go in the schema or the surrounding agent prompt. The description is about *what* and *when*, not *how*.

## What A Strong Description Covers, In Order

A well-formed description for a non-trivial tool moves through these in order:

1. the function — what the tool does, as a verb on a resource
2. any precondition on the input — what must be true of the arguments for the call to make sense
3. what the return value carries, and in what units or form
4. the trigger — when the model should reach for it
5. what the tool does *not* do — the boundary that keeps it from being over-used

A weak description leaves the model guessing about every one of these. Cover them from the actual tool rather than from a template: the order is the lesson, not any particular wording.

## Checklist

For each tool description being written or rewritten:

- [ ] first sentence answers "what does this do?" as a verb on a resource
- [ ] the trigger condition for calling the tool is named explicitly
- [ ] the anti-trigger is named, especially relative to nearby lookalikes — and the lookalikes' own descriptions point back
- [ ] every side effect (state writes, user-visible output, external calls, irreversibility, cross-session or cross-agent effects) is documented
- [ ] caveats, limitations, and what the tool does *not* return are stated
- [ ] the description is at least 3-4 sentences, longer if the tool is complex
- [ ] no marketing voice and no opaque jargon without gloss
- [ ] no concrete worked example values in description prose (they anchor)
- [ ] every parameter has its own `description:` covering meaning, units/format, valid values, default behaviour, and where the value comes from
- [ ] every enum parameter lists its values inline with an explanation of when each applies
- [ ] every required parameter is in the schema's `required` list, and every optional parameter's default behaviour is stated
- [ ] the description has been re-read against a new-hire framing — would someone with no context understand what to do?
