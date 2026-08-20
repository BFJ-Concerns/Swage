# The Alignment Handshake

A protocol for verifying that a **receiver's** understanding matches an
**issuer's** intent. It is self-contained by design: the commission skill
applies it between user and model (level 1), and an orchestrator borrows it
unchanged for worker restatements (level 2). The invariants hold at both
levels; only the application notes diverge.

## Invariants

1. **The receiver externalises; the issuer corrects.** Never "do you
   understand?" — always "here is my understanding; break it." The party
   holding the intent is the only one who can falsify a reading of it.

2. **Transformation, not repetition.** The restatement uses different words
   and is *more concrete* than the original. Abstract restatement is
   compatible with both the right and the wrong understanding; concreteness
   forces divergence to surface.

3. **Projection beats description.** Restate what you will *do* —
   implications, an approach sketch — not just what was said.
   Misunderstanding hides inside accurate descriptions and shows up
   immediately in plans. This is the most load-bearing rule in the protocol:
   when judging a restatement, weight the projection, not the paraphrase.

4. **Small units, never batched.** Batched restatement invites a batched
   "yes" that verifies nothing. Verify items as they settle, one at a time.

5. **Corrections re-verify and cascade.** A corrected understanding is
   restated once more. The correction also invalidates the verified status of
   anything settled on the old understanding — sweep the dependents and
   re-verify what the correction touched.

6. **Convergence is the surprise test.** Done when projections stop producing
   corrections.

## Level 1: User ↔ Model (commission conversations)

The model is the receiver; the user issues intent — much of it tacit, which
the plain handshake cannot reach. Two adjustments:

- **The elicitation half.** Restatements deliberately extend into inferred
  territory to make the tacit falsifiable: assert your inferences about what
  the user never stated — deployment surface, audience, scale, lifetime,
  whatever the conversation has left implicit — concretely enough to be
  contradicted. A wrong inference corrected cheaply now is a wrong assumption
  that never reaches the contract.

  **Own the inference; never source it to the user.** The boldness lives in the
  *content* of the inference, not in falsely attributing it. "My read is you
  chose X for its maintenance — break it" makes the inference falsifiable *as an
  inference*; "your stated reason is X" launders a guess into the user's record,
  and the moment it survives unchallenged it becomes a fabricated fact in the
  contract. An asserted inference and a quoted-back user declaration are
  different speech acts — keep them distinct. Never attribute to the user a
  reason, motive, or decision they did not state; phrase it as your reading
  awaiting confirmation, and it stays marked as inferred in the draft until they
  confirm it (see *Inferences are owned, not attributed* in `SKILL.md`).

  This bites hardest right after a correction. Invariant 5 says a correction
  cascades — and a user who has just said "none of those are right" is the worst
  moment to assert a fresh rationale as fact. Fresh rationale straight after a
  rebuke is the highest-risk inference there is: offer it explicitly as a new
  guess to be shot down, never as something already established.

- **The role flip.** When the model is the issuer — teaching, correcting a
  misconception, proposing a simplification — the user cannot be made to
  restate. Check uptake lightly through their subsequent usage: do their next
  messages reflect the corrected understanding, or quietly revert?

## Level 2: Orchestrator ↔ Worker

Workers can be made to restate as routine: deliver the brief at a path, then
"restate the brief in your own words and sketch your approach before
starting."

- Judge the response against invariants 2 and 3: an accurate paraphrase
  verifies nothing; the approach sketch is where misalignment surfaces.
- On a correction, redirect the worker and require a fresh restatement of the
  changed part (invariant 5) — don't accept "understood" as closure.
- Convergence per invariant 6: the worker's projections stop producing
  corrections; then leave them alone.

## The Bar (Anti-Ceremony Guard)

Restatement is not a verbal tic after every exchange. The bar: anything to be
recorded as settled — written into a contract, accepted as a brief — is
verified first; trivia below that bar is not.
