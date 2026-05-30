# Test value and level

## The concern

Tests are an asset only when they earn their maintenance cost. This brief judges
whether each test added or changed on the branch is worth keeping and sits at the
right level — not whether the code it covers is correct (that is `/code-review`'s
job), and not whether enough tests exist (a separate concern).

A test earns its place when it would catch a real regression in observable
behaviour, and when it is the cheapest honest way to do so. Tests that fail that
bar are not free: they slow the suite, break on harmless refactors, and — worst —
look like coverage while testing nothing. Flag them.

## What good looks like

- The test asserts on observable behaviour or a contract, not on internal
  representation. The co-located tests in `swage-agent/src/expiry.rs` are the
  shape: stand up real `SqliteStorage::in_memory()`, exercise the function,
  assert the count that callers would actually observe.
- A regression in the thing under test would make the assertion fail, and a
  harmless refactor would not.
- Behaviour that only emerges across crate boundaries (turn loop, gateway →
  agent → storage → provider) is exercised through the integration suites under
  `crates/*/tests/` or through `Agent::process_turn_streaming`, not faked with a
  pile of mocks in an isolated unit test.

## What to flag

**Deletion-only tests.** A test whose entire purpose is to assert that something
no longer exists — `assert!(x.is_none())` on a field that was just removed, a
test named for the *absence* of a removed variant, a guard that a deleted code
path stays deleted. The compiler and the surviving behavioural tests already
enforce this; the test adds upkeep and no signal. Flag it and say what positive
behaviour, if any, ought to be tested instead.

**Fragmented unit tests that want to be one integration test.** Several brittle,
near-identical unit tests each rebuilding the same scaffolding to assert one
slice of what is really a single end-to-end flow. When the behaviour only means
anything as the pieces interact, one integration test through the real path is
more honest and less brittle than five isolated ones. Point at the existing
integration suite that is the natural home.

**Over-mocked tests that exercise the mock.** So much is stubbed that the
assertion can only confirm the test's own setup, not the code under test. This is
the more serious failure: it reads as coverage but verifies nothing.

**Tests that cannot meaningfully fail.** Tautological assertions, "it didn't
panic" where a returned value should have been checked, or assertions on a
constant the test itself defined.

**Redundant tests.** A new test that covers exactly what an existing one already
does, adding suite time without adding a distinct failure it would catch.

## Out of scope

- Correctness of the code under test, or whether new code is missing tests — not
  this brief.
- Test naming, formatting, or file placement conventions — mechanical.
- Flakiness, timing, or test-infrastructure gaps.

## Severity

A tautological or over-mocked test that masquerades as coverage is the serious
case — it actively misleads about what is verified. Deletion-only and merely
redundant tests are usually minor maintenance drag; flag them, but don't treat
them as blocking unless they obscure a genuine coverage gap.
