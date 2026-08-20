# Misconception-Hunting Moves

The dispatched moves the hunting posture fires, their information diets, the
reconciliation beat for returned findings, the relay discipline, and the
hard requirement on the Ensemble runtime. Read this whenever a signal has
fired and a move is about to run, and again when its findings return;
`SKILL.md` carries the always-on posture and the signals themselves.

**Moves** — selection and grounding for in-context verification in
[dispatched-agents.md](dispatched-agents.md); how to run
the cross-engine workflows in
[ensemble-workflows.md](ensemble-workflows.md):

1. **Verify, don't transcribe.** Checkable claims get checked before they
   enter the README. This is fact-checking, not divergent generation, so it
   stays lightweight and in-context. One class is load-bearing by default: a
   mechanism claiming a security or soundness property earns a few minutes'
   hostile reasoning about how the claimed property could fail, before it
   settles. It is the cheapest check the contract will ever get, and a property
   nobody attacked is a property nobody verified.
2. **Divergent survey** — when an important option space is unwalked or an
   anchor is in play, run the `divergent-survey` workflow: a research stage
   surveys the outcome's published theory, proposers blind to the anchor — half
   seeded with disjoint slices of the research, half from the brief alone —
   generate genuinely different approaches, the opposite engine critiques
   each, and both engines surface a *ranked* comparison. The ranked
   options go to the user with their evidence and the decisive questions, and
   you talk them through together — the main session never silently adopts a
   winner.
3. **Blind re-derivation** — when the subject is a whole design rather than one
   open decision, run the `blind-rederivation` workflow: a designer isolated
   from the incumbent by construction re-derives the design from first
   principles — deriving the concerns themselves, not answering an agreed
   question — and the result is read against what exists for convergence
   (independent validation) and divergence (the assumptions the incumbent
   never re-examined). The two blind tools split by subject: the *survey* for
   one open decision, *re-derivation* for a design taken whole. Offer it
   proactively at its natural moments — greenfield capture once the user's
   intent is extracted (their intent and the blind derivation are two
   independent designs of the same thing, compared like two model families'
   blind output), a major new feature, a settled area being revisited or
   suspected of a wrong starting place — with your own recommendation either
   way, and the user decides; it is never a gate. Not worth the spend for
   small decisions (the survey fits), or when there is neither a settled
   artefact nor extracted intent to compare against.
4. **Draft attack** — near sign-off, scaled by delta, run the `draft-review`
   workflow against the assembled draft, sighted on the settled Decisions.

**Per-stage information diet.** Each move sees a deliberately different slice,
and that is what makes it work: survey proposers never see the anchor; the
re-derivation's designer never sees the incumbent; the draft attack is sighted
on the settled Decisions so it cannot manufacture phantom gaps; the
preservation passes split blind detection from sighted adjudication. The
workflows enforce the diets by what each is fed — but never hand a blind stage
the anchor, and never run the attack blind to the decisions.

**The reconciliation beat.** A returned finding is a hypothesis, not a verdict.
Before acting on any of them, reconcile each against what you and the user have
already settled this conversation:

- A finding that collides with an **examined** Decision (alternatives were
  weighed) is answered from the settled design first. It reopens only on
  genuinely new information the decision never had — and then you renegotiate
  openly with the user, you never quietly reverse it inside a fix.
- A finding that collides with a **provisional** Decision (named once, never
  compared) is fair game: surface it for joint reconsideration rather than
  shielding it.
- Only a genuine gap the design never addressed becomes a fix.

This is the guard against the failure where a fresh, confident finding from a
context-blind reviewer outweighs older settled reasoning and the session
contradicts its own earlier calls.

**Relay discipline** — the main session's half of grounding (the dispatched
work's Grounding Rules are the other half: it grounds its output, you keep it
grounded on the way back):

- Pass evidence **verbatim** — paraphrase drift becomes fact downstream.
  Intent gets *transformed* because the issuer is present to falsify it;
  evidence gets *quoted* because nobody is present to correct drift. When a
  finding needs the user's call, quote the complaint or summarise it
  faithfully with the quoted evidence attached — never ask the user to decide
  on an unseen review.
- Never present an ungroundable challenge.
- **No hunting quota** — if nothing is wrong, the correct number of
  challenges is zero.
- Relay findings with their provenance — they come from a context-blind
  reviewer and carry confident noise alongside genuine hits, so flag each as
  such. Resolve the doubtful ones by checking, not by quietly dropping what
  looks like noise: a discarded finding is a missed misconception, and the
  user-present handshake is the filter that belongs here. Drop one only when
  evidence already in hand contradicts it.
- **Log every finding's disposition, but relay only the material ones** — "no
  silent drop" is not licence to bury the user in low-value caveats; the
  workflows already hold the minor and redundant findings to one side.

**Hard requirement.** The divergent survey, the blind re-derivation, the draft
attack, the preservation passes, the shedding run's grounding (Beat 2), and
the pre-sign-off commitment audit run
on the cross-engine Ensemble runtime (the ensemble-workflow skill's launcher),
and the moves need it
live (see [ensemble-workflows.md](ensemble-workflows.md)).
When a *triggered* move cannot reach it, **stop and tell the user** — never
stand up hand-prompted same-model sub-agents in a workflow's place on your own
initiative: that forfeits the differently-trained judgement the move exists for,
and a half-blind survey dressed as cross-engine is worse than none. What runs
instead is decided with the user, openly: wait for the ensemble; drop the move,
recording it as not performed (the Stop condition then carries that record —
see Sign-off in `SKILL.md`); or — for the draft attack and the preservation passes only — a
clearly-labelled **degraded review** under the contract in
[ensemble-workflows.md](ensemble-workflows.md). The
survey has no degraded rendering — its value *is* the other engine's
divergence — and neither does grounding: a shed happens only on the live
cross-engine confirm, so would-shed candidates stay fully elaborated until it
can run. The re-derivation has none either: its designer must be a fresh,
isolated context, which the main session — already steeped in the incumbent
and the conversation — cannot impersonate. A reported degraded review beats both a skipped gate and a fabricated
pass. (A round that touches no product intent triggers no such move — but any
round that works the What owes the commitment audit, so capture and feature
updates now depend on the ensemble being live; its degraded rendering is in
[ensemble-workflows.md](ensemble-workflows.md).)
