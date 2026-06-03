# Output Patterns

Use these when a skill needs to shape what the agent produces — a structured report, a tool-call payload, a status message, or a piece of prose.

## Template Pattern

Provide a template when the output's shape matters and the agent would otherwise vary it. Match strictness to need.

**Strict — when downstream parsing depends on it:**

```markdown
## Status update format

Use this exact shape:

**Doing:** [one short clause naming the current step]
**Done:** [what changed this turn, one per line]
**Next:** [the decision or question that now stands]
```

**Loose — when adaptation is useful:**

```markdown
## Status update format

A useful default is doing / done / next. Adapt the sections when the
situation does not fit that shape.
```

In the strict case, the agent follows the literal structure. In the loose case, name the goal so the agent can pick a fitting variation.

## Principle Pattern

When the output is open-ended — prose, explanation, a judgement call — describe the qualities a good output has rather than templating it. Templates flatten voice; principles steer it.

```markdown
A good review comment names the file and line, states what is wrong and
why it matters, and proposes a concrete change. It does not restate the
code or pad with praise.
```

This style is harder to follow than a template, but the result reads as one considered response rather than a filled form. Use it when judgement matters more than uniformity.

## Examples Pattern

Prefer principles over examples in the body. Concrete examples become defaults the model reproduces — the project preference is to state principles abstractly and let the agent generate the specific text from them.

Exceptions:

- structural conventions (folder names, frontmatter shape, fixed identifiers) where the value *is* the literal string
- output formats whose shape is parsed downstream and would fail without the literal structure

If an example genuinely earns a place in the body, mark its role explicitly so the agent treats it as a constraint, not as the canonical output to reproduce. Remember the whole body loads on every activation, so a long worked example is paid for on every use — keep examples to the minimum that pins the constraint.
