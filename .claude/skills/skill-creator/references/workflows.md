# Workflow Patterns

Use these when a skill's body describes a multi-step procedure the agent should walk through.

## Sequential Workflows

For procedures with a clear order, list the steps directly:

```markdown
Reviewing a change follows these steps:

1. Read the changed files in full before judging any line.
2. Check each change against the project's correctness standards.
3. Note issues with the file and line they occur at.
4. Group the findings by severity.
5. Summarise the review, leading with anything that blocks merging.
```

Keep the list short. If a step itself needs explanation, give it one sentence underneath rather than nesting another list.

## Conditional Workflows

When the procedure branches, surface the decision points explicitly:

```markdown
1. Identify the kind of request:
   - **A bug report?** Reproduce it before proposing a fix.
   - **A feature request?** Confirm the scope before writing code.
   - **A question?** Answer from the code; do not change it.

2. Run the chosen path to completion before switching to another.
```

Branching skills are often a sign the skill is doing two jobs — consider whether each branch should be its own skill the user activates separately, since the runtime loads the whole body either way.

## Feedback Loops

When a step needs to validate its own output before continuing, write the check into the loop rather than hoping the agent self-validates:

```markdown
1. Draft the output.
2. Re-read it against the stated constraints.
3. If any constraint is unmet, revise and check again.
4. Only present the output once it passes.
```

The explicit "check again" line is what makes the loop hold. Without it, the agent often runs the check once and moves on regardless of the outcome.

## When Not To Use A Workflow

Workflows fit skills whose value comes from sequencing. Skills whose value comes from principle (style, tone, taxonomy, framing) read better as prose, not numbered steps. Forcing principle into a workflow makes the body longer and the guidance less true.
