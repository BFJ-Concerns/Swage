# Tool design

What a tool takes, returns, fails with and may be called by. The text the
model reads is covered in `description-writing.md`.

## One tool or several

Each extra tool is one more candidate the model has to tell apart, and each
extra mode on a tool is one more way to fill in its arguments wrongly. Choose
by which mistake is the likelier.

One tool with an `action` or `kind` parameter suits operations that share
their caller, their permissions and their return shape, and differ only in
the verb or the kind of thing acted on. Separate tools suit operations whose
required fields differ, whose side effects differ in kind (one reads, one
writes, one spends money), or whose permissions differ, since a permission
entry names a tool and cannot name one mode of it.

## Names

Built-in names are snake_case, the resource first and the action after it
(`file_read`, `file_write`, `web_search`), so that tools on one resource
share a prefix. Follow that. A bare verb gives the model nothing to choose
by, and a name that suggests one action while the description states another
costs trust in both. A name the model sees should say what the tool does and
not which component implements it.

## Parameters

`Tool::parameters()` returns a JSON Schema object, and dispatch validates
every call against the whole of it before `execute` runs
(`crates/swage-tool/src/validation.rs`): required fields, types, `enum`,
bounds, `additionalProperties` and the rest. So:

- State a constraint in the schema and `execute` need not check it again. A
  closed set of values is an `enum`; an object that takes no other keys says
  `additionalProperties: false`.
- A check the schema cannot express goes in `validate_call`, which runs
  before any work and must do no I/O, or in `execute`. Either returns
  `ToolError::InvalidParameters` with a message that says what to change.
- A schema that is not valid JSON Schema fails every call to the tool.
- A provider adapter may send the model a reduced form of the schema, so a
  constraint the model must respect is also stated in that parameter's
  description.

Name a parameter for exactly what it holds, so that a label cannot be passed
where an identifier is wanted. List the required parameters in `required`.

## Return value

`execute` returns `Result<Value, ToolError>`, and the `Value` is what the
agent acts on next. The framework adds timing and cost around it.

- Return what the agent's next step needs and leave out bookkeeping.
- Return an identifier the model can recognise. Where the real identifier is
  opaque, return it with a readable label beside it.
- Tools that do parallel things return parallel shapes.
- Say what the call changed: the path written, the record created, what is
  now visible to the person.
- Dispatch scans the serialised return value for credential-shaped strings
  and redacts them before the model sees it, so a return shape cannot depend
  on such a string arriving intact.

## Errors

A `ToolError` goes back to the agent's turn loop, which decides whether to
retry, try another tool or report the failure. Choose the variant that
carries what happened:

- **`InvalidParameters { name, message }`**: the arguments were wrong. It is
  permanent for this call, so `message` says what was wrong and what to pass.
- **`ExecutionFailed { name, message }`**: the tool ran and failed. This and
  `Timeout` are the two variants `is_retriable()` returns true for, so say in
  `message` whether a retry could succeed.
- **`Timeout { name, duration }`**: the call ran past its limit.
- **`NotFound`** and **`PermissionDenied`** come from the framework. A tool
  does not return them for its own purposes.
- **`RequiresApproval`**, **`RequiresLocationApproval`** and
  **`RequiresDelegation`** are control flow, not failures. The turn loop
  intercepts them to run an approval or a delegation. A tool returns one only
  when it means to start that mechanism, as the delegate tool does with
  `RequiresDelegation`.
- **`ExecutionUncertain`** and **`AfterExecution`** wrap another error to
  record whether the work may have happened. The framework applies them.

An error message is read by the model. Name what was wrong and how to fix
the call, and leave out stack traces.

## Permissions

Which tools an agent may call is its `PermissionConfig`
(`crates/swage-tool/src/permission.rs`), resolved for the agent from the
configuration in force and its definition. `check_permission` decides each
call in this order:

1. A name in **`deny`** is refused, whatever else lists it.
2. The tool must be granted by **`allow`**: by `"*"`, by its own name, or by
   the name of a bundle that contains it. A tool nothing grants is refused.
3. A granted tool that **`require_approval`** lists, by name or through a
   bundle, waits for a person's approval. An entry may carry a timeout and
   what happens when it runs out. Listing a tool there does not grant it.
4. Otherwise the call is allowed.

`bundles` map a label to a list of tool names. A bundle's label can stand
for its tools in `allow` and `require_approval`. `deny` matches tool names
only, so denying a bundle's tools means listing each of them. A tool that
joins an existing family goes into that family's bundle, so that every agent
granted the bundle receives it.

Three things do not grant a tool. A skill's `tools:` front matter records
what the skill expects and authorises nothing. A workflow's or a
delegation's tool list only narrows what the dispatching session already
has. And a tool whose `platform_holder()` names an agent is withheld from
every other agent before permissions are consulted, wildcard included.

For a tool that is dangerous or cannot be undone, decide whether it should
sit behind `require_approval` by default.

## Secrets and where the tool runs

- **`sensitive_keys()`** names the parameter keys whose values are replaced
  with `[REDACTED]` in logs and hook output (`redact_params` in
  `params.rs`). `execute` still receives the real value. Set it for any
  parameter that carries a token, a password or another secret. It covers
  the input; the scan of the return value is separate.
- **`device_secret_keys()`** names the parameter keys that may hold a
  reference to a registered secret. The gateway resolves the reference at the
  host boundary, so the tool receives the secret and the model only ever
  holds the reference. Use it when the tool needs a real credential.
- **`requires_device_execution()`** returning true keeps the tool in the
  gateway process. The skill's *Wiring a tool* says when a tool must, and
  what a tool that does not must also be added to.

The first two name parameter keys, and the framework does the redaction and
resolution.

## Changing a tool that exists

Say what its callers rely on before changing it. When tools are merged or
split, each old name is removed everywhere in the same change, and each new
tool's permissions and bundle are decided before it is written. Then go
through *Wiring a tool* for every name that changed.
