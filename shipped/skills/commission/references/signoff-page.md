# The Sign-off Read

The sign-off read is delivered in the conversation: the change to be
signed, led by its decision cards, with the user's reply as the verdict.
It is the delivery surface for the read the Sign-off doctrine in
`SKILL.md` requires; everything about *when* the read happens and what
must have cleared first is unchanged and lives there.

The read is shaped for the reader's attention, not for completeness-first:
it opens with the few decision points that genuinely need the user's
judgement and how big the ask is, with the full change following. A read
that opens on the whole diff invites a cursory skim of everything
instead of a real read of what matters — the layout does the triage so the
reader doesn't have to.

## Build the Read

Build the content from git, so the read shows what will be committed:

- **Update** (any sign-off where a prior committed contract exists — an
  update conversation, a hold, a release, a withdrawal, a correction): the
  object is the diff. New `held/` and `shed/` files are untracked until the
  closing commit and a plain `git diff HEAD` omits them, so register intent
  first, then diff — from the annexe root:

  ```sh
  git add -N README.md VERIFICATION.md held/ shed/
  git diff HEAD -- README.md VERIFICATION.md held/ shed/
  ```

  Check the diff before presenting it: it is wrong if it omits any file the
  closing commit will touch. Offer the full draft README alongside when the
  diff alone would leave the standing text unseen.
- **Initial capture** (no committed README yet): the object is the file
  itself, read whole.

**Write the decision points before presenting.** Distil the round into the
few assembly-level items that most deserve the user's attention in the final
read — a Decision settled or retired this session, a proposed choice they
merely accepted, anything where a wrong call costs something real — ranked by
stakes: each item a plain-language headline, then two or three sentences
ending with the cost of a wrong call. The cards
re-present what the conversation already settled; nothing reaches the read
unsettled — in particular a cap, budget, or ceiling must already have its own
conversational answer (the caps rule in `SKILL.md`) before it may appear as a
card. Leave out minor items the handshake fully closed — with three exceptions
that are always cards, however small they read: a claim the preservation
review's gains direction surfaced as an unsettled
addition and the round then settled — it entered the draft without a
conversational moment, so the read is where the
user actually sees it; each want the commitment audit's exclusion diff
surfaced that the round settled as a deliberate exclusion, so the user signs
over what was left out as well as what was written; and each hollow verdict
the round accepted as a residual — sharpening it would demand
over-specification the surprise test forbids — carrying the verdict's cheap
reading and the acceptance reason, so the signature is what closes it. Write
each such card with what the decision changes and the cost of a wrong call,
like any other. A genuinely decision-free round (a pure wording tidy) opens
on the artifact instead.

**Tags are final before presenting.** Every Decision's examined/provisional
tag is set by what actually happened in conversation — compared or merely
named — before the read goes out. The verdict never changes the draft: after
any approval the only thing that varies is what the commit message records,
so the user always signs exactly the bytes that land.

Present the read — decision cards first, sized ask stated, then the diff or
file — as the final message of its turn, together with the sign-off report,
and wait. Only the user ends a sign-off: by answering, or by saying to stop
waiting. Silence is not a verdict.

## Act on the Verdict

The user's reply carries the verdict. Read the comments before acting on
the headline answer — the comments are the user's words and outrank it:

- **A yes, comments empty or advisory** → the sign-off yes. Land the
  closing commit and push as the Sign-off doctrine directs, approval recorded
  in the message.
- **A yes given on a skim** (the user says they only skimmed, or their yes
  doesn't say how they read and Decisions were settled this round — ask
  which it was) → also the sign-off yes — land and push the same unchanged
  draft, noting the skim in the closing
  commit message. The tags already reflect the conversation (set before
  presenting), and a skimmed approval never upgrades one: it is consent, not
  comparison, so nothing it covered gains the reopen-shield examined status
  carries. Accept the verdict without re-asking — an honest light read beats
  a pretended full one. Comments carrying an exact correction or a
  substantive change are classified exactly as under a full yes: how the
  user read and what they want are separate, and the comment
  branches below apply regardless.
- **A yes whose comments supply an exact correction verbatim** — replacement
  text that is literally the user's own pen — → apply it and land with the
  approval. The moment applying a comment requires composing words, however
  few, it is not this case: treat it as a revision request.
- **A yes whose comments ask for substantive change** → a revision round
  wearing the wrong headline. Treat it as a revision request.
- **A revision request** → the comments come back into the conversation as
  user input: work them through the normal handshake, revise the draft,
  **rebuild the diff from git** (the same intent-to-add and diff commands as
  above — a round that reuses the pre-revision diff shows the user a read
  without the very change they asked for), then present the next round's
  read the same way, its decision cards updated for the revision.
