# Tool Design

Design choices for Swage runtime tools. Read this when adding a new tool, consolidating or splitting existing ones, or restructuring parameters and return values. Pair it with `description-writing.md` for the description text itself.

This file gives criteria, not a catalogue. Decide each choice from the properties of the tool in front of you rather than matching it against a named example — example tool names anchor the model toward shapes that may not fit, and they go stale as the catalogue moves.

## One Tool Or Several

The default mistake is splitting one logical operation into a cluster of lookalike tools distinguished only by a noun in the name. The model then has to pick among near-identical siblings, and selection ambiguity climbs with every one added.

**Prefer one tool with an `action` or `kind` discriminator parameter when:**

- the siblings share the same caller pattern, the same permission expectations, and the same return shape
- the only meaningful difference between them is a single discriminator — the kind of thing being acted on, or the verb being applied
- the model frequently picks the wrong one from the cluster

**Prefer separate tools when:**

- the parameter schemas genuinely diverge (different required fields, different types, different validation)
- the side effects differ in kind (one reads, one writes, one calls out to a paid external service)
- the permission story differs (one should be freely allowed, another gated behind approval) — separate tools let the permission config target each independently

Consolidation reduces selection ambiguity; splitting reduces parameter ambiguity. Pick the lever that matches the actual failure mode.

## Naming And Namespacing

Names are the second signal the model sees, after the description, when deciding which tool to call. They earn their keep when they make selection unambiguous at a glance. Match the established convention of the built-in tools rather than inventing a new style.

- **Use snake_case.** Every built-in tool name is snake_case (`file_read`, `web_search`, `python_execute`). A new tool that breaks the casing convention reads as foreign and costs the model a beat.
- **Group related operations under a shared resource prefix.** The built-in `file_*` family — `file_read`, `file_write`, `file_list`, `file_search` — is the model for this. When several tools act on the same resource, a shared prefix groups them and signals the domain at a glance. The established convention is resource-or-namespace first, then the action.
- **Avoid generic verbs alone.** A bare `update`, `get`, or `do` forces the model to disambiguate from context. Pair the action with the resource it acts on.
- **Match the name to the description.** If the name implies one action and the description states another, the mismatch costs the model trust in both.

When renaming, every reference to the old name must update in the same change: the `Tool::name()` return, the registry registration, the protected-names list if it is a built-in (`PROTECTED_NAMES` in `registry.rs`), any permission config entry or bundle that lists the tool, any skill `tools:` list that mentions it, and any test that enumerates the catalogue. See the wiring checklist below.

## Parameter Schema

The schema is the model's contract with the tool. It is declared by `Tool::parameters()` returning a JSON Schema object (`{"type": "object", "properties": {...}, "required": [...]}`). Treat each field as deliberate.

- **Use specific names.** Name the entity and its form, so the model cannot pass a label where an identifier was wanted, or an object where a string was wanted. A parameter named for exactly what it holds removes a whole class of wrong-argument errors.
- **Use enums for closed value spaces.** If a field accepts only a fixed set of values, declare them as a JSON Schema `enum`. The model treats enumerated values as authoritative; a freeform string description of the valid values is noticeably weaker. But the schema `enum` is for the model's benefit only — Swage's validator does not enforce it (see below), so also reject out-of-set values inside `execute()`.
- **State units and formats in the parameter description.** `type: string` says nothing useful by itself. Whatever constrains the value — its unit, its encoding, its format — belongs in the description, stated as a property the model must satisfy.
- **Mark required fields.** List required parameters in `required`. For an optional parameter, the description must state what happens when it is omitted.
- **Know exactly what the validator enforces — it is a narrow subset.** Swage's dispatch pipeline validates parameters against the schema before `execute()` runs (`validation.rs`), but it checks only required-field presence and coarse type matching, recursively over objects. It does **not** enforce `enum`, numeric bounds (`minimum`/`maximum`), `items` for arrays, `additionalProperties`, `oneOf`, `allOf`, or `$ref` — and it treats `"integer"` as any JSON number, so a float passes an integer field. Declare these constructs for the model's guidance, but treat them as undefended: validate every closed value, numeric bound, and array-element constraint that actually matters inside `execute()` and return `ToolError::InvalidParameters` (the built-in `memory` tool does this for its `operation` field). A constraint that exists only in the schema is documentation, not a guard.

If a parameter shape is complex (nested objects, format-sensitive strings), describe the shape in the parameter description. `definition()` forwards only `name`, `description`, and `input_schema` to the provider — there is no `input_examples` channel — so do not rely on schema-level examples, and do not substitute fake values in the description prose to compensate (the model reproduces them). If a tool genuinely needs worked example inputs, flag adding an examples channel to `ToolDefinition` as the real fix rather than baking examples into prose.

## Return Shape

`execute()` returns a `Result<Value, ToolError>`; the raw `Value` is what shapes the agent's next move. (The framework wraps it in a `ToolOutput` that also carries duration and optional cost — you return only the payload `Value`.) Bloated returns cost context for no value; opaque returns force the model to guess.

- **Return semantic identifiers, not opaque ones.** A slug or name the model can recognise beats an opaque key it cannot reason about. Where the canonical identifier is opaque, return both — the identifier for round-tripping, a human-readable label for reasoning.
- **Return only what the agent's next step needs.** Strip internal bookkeeping, debug counters, and framework metadata. If a downstream tool needs them, that downstream tool can fetch them.
- **Be consistent across siblings.** Tools that do parallel things should return parallel shapes. The model learns return-shape patterns, and inconsistency between siblings erodes that learning.
- **Surface side effects in the return.** If the call wrote to disk, include the path. If it emitted something to the user, include a marker. The agent needs to know what is now visible.
- **Remember output is scanned.** The dispatch pipeline runs leak detection over the serialised return value before it reaches the model. Do not design a return shape that depends on a credential-shaped string surviving verbatim — it may be redacted.

## Error Messages

Errors returned from `execute` are `ToolError` values (defined in `crates/swage-core/src/error.rs`). They flow back to the agent's turn loop, which decides whether to retry, fall back to another tool, or surface the failure. Make that decision possible, and pick the variant that carries the right meaning:

- **`InvalidParameters { name, message }`** — the arguments were malformed or failed a constraint. Permanent for the current call; the model must fix the arguments to recover. Put the specific problem and the fix in `message`.
- **`ExecutionFailed { name, message }`** — the tool ran but failed (an external command errored, a transient fault). This is the retriable variant (`is_retriable()` returns true for it and for `Timeout`). Say in `message` whether a retry could plausibly succeed.
- **`Timeout { name, duration }`** — execution exceeded its budget. Also retriable.
- **`NotFound`, `PermissionDenied`** — generally produced by the framework, not by your `execute`. Be aware they exist so you do not duplicate their job inside the tool.
- **`RequiresApproval` / `RequiresLocationApproval` / `RequiresDelegation`** — these are control flow, not failures. The turn loop intercepts them to run an approval gate or a delegation. A tool returns one of these only when it is deliberately driving that mechanism (the delegate tool returns `RequiresDelegation`); ordinary tools do not.

Be specific and actionable: an error that names what was wrong and how to fix the call lets the model recover; an opaque code tells it nothing. Never return raw tracebacks — they cost context and tell the model nothing it can act on.

## Permissions: Which Agents Can Call The Tool

Swage has no "surface" concept. Which tools an agent may call is governed by its `PermissionConfig` (`crates/swage-tool/src/permission.rs`), resolved per agent from the config cascade. The dispatch pipeline checks every call against it. The model is default-deny: a tool the config never allows is simply unreachable, no matter how good its description.

The resolution order is fixed (`check_permission`):

1. **`deny`** wins outright — a denied tool is never reachable, even if also allowed or approval-gated.
2. **`require_approval`** — the tool is permitted but gated behind a human approval (returns `RequiresApproval`). Entries may carry a per-tool timeout and timeout policy.
3. **`allow`** — `"*"` (wildcard) or the tool name directly, or a bundle name that expands to include it.
4. Otherwise **default-deny**.

`bundles` map a label to a list of tool names; a bundle name used in `allow` or `require_approval` expands to its members at resolution time. This is the Swage analogue of grouping related tools — define a bundle (for example a filesystem bundle covering the `file_*` tools) and grant the bundle rather than enumerating tools per agent.

When adding or restructuring a tool:

- decide which agents should reach it, and whether through a direct allow, an existing bundle, a new bundle, or an approval gate
- if the tool belongs to an existing family that already has a bundle, add it to that bundle so every agent granted the bundle picks it up
- if the tool is dangerous or irreversible, consider `require_approval` rather than a bare `allow`
- a Swage skill's `tools:` frontmatter list is **informational only** — it documents what a skill expects but does not grant anything. The agent's `PermissionConfig` is the only thing that actually authorises a call. If a skill relies on a tool, make sure the agent's permission config allows it.

## Sensitive Keys And Host Secrets

Two trait methods govern how a tool's secret-bearing parameters are handled. Both default to empty.

- **`sensitive_keys() -> &[&str]`** names the parameter *keys* whose values should be redacted in logs and hook output. The framework replaces the value with `[REDACTED]` for the logging copy (`redact_params` in `params.rs`); `execute()` still receives the real value. Set this for any parameter that carries an API token, password, OAuth credential, or other secret payload. This is distinct from output leak detection (which scans the *return value* via `swage-safety`'s `LeakDetector`); `sensitive_keys` covers the *input* side.
- **`host_secret_keys() -> &[&str]`** names parameter keys that may hold secret *references* the gateway resolves at the host boundary before execution. A tool that opts in can receive an injected raw secret without exposing it to the model or across the sandbox IPC. Use this when the tool needs a real credential to do its work but the model should only ever see a reference to it.

A related method, **`requires_host_execution() -> bool`**, forces the tool to run in the gateway process rather than the sandbox child. Set it when the tool needs gateway-only state, host networking, or typed errors the sandbox IPC layer cannot preserve. Conversely, a tool that does *not* set it can be routed to the sandbox child, which runs its own separately-built tool list — so it must also appear in the sandbox catalogue (see the *Wiring Checklist*).

The lists name parameter *keys*, not values. The framework handles redaction and resolution.

## Wiring Checklist

When adding a new tool, every entry in this list must be updated in the same change:

- [ ] the `Tool` impl exists and compiles
- [ ] the tool is registered with the `ToolRegistry` — `register_builtin` for a Swage built-in (which bypasses the protected-namespace guard), `register` for a user/extension tool (which is rejected if it collides with a protected name)
- [ ] if it is a built-in, its name is added to `PROTECTED_NAMES` in `crates/swage-tool/src/registry.rs` so user tools cannot shadow it
- [ ] if the tool can run in the sandbox child (it does not set `requires_host_execution()`), it is added to the **sandbox tool catalogue** as well — `build_sandbox_agent_tools` in `crates/swage-gateway/src/main.rs` builds the sandbox child's tool list separately from the gateway registry, so a tool registered and advertised in the gateway but missing here will pass schema and permission checks and then fail at execution in sandbox mode. A host-only tool must instead deliberately set `requires_host_execution()` so it never routes to the child.
- [ ] the permission model lets the intended agents reach it — a direct `allow`, membership in a granted bundle, or a deliberate `require_approval` entry
- [ ] any skill that relies on the tool lists it in `tools:` for documentation, and the agents activating that skill have permission config that actually allows it
- [ ] any test that enumerates the tool catalogue or the protected namespace has been updated
- [ ] `sensitive_keys` / `host_secret_keys` / `requires_host_execution` are set if the tool handles secrets or needs host execution

## When Restructuring An Existing Tool

- name the behaviour that must be preserved before changing anything
- for a consolidation, decide what happens to the old tool names — alias, removal, or deprecation — and update every permission entry and protected-name list that referenced them
- for a split, decide which agents (and bundles) each new tool belongs to before writing them
- rerun the wiring checklist for every name that changed
