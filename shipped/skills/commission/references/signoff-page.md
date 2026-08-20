# The Sign-off Page

The sign-off read is delivered as a Wrack ephemeral page: a private
capability-URL page carrying the change to be signed, a comments box, and
three buttons — **Sign off**, **Signed on a skim**, and **Request changes**.
The page is the delivery
surface for the read the Sign-off doctrine in `SKILL.md` requires; everything
about *when* the read happens and what must have cleared first is unchanged
and lives there.

The page is shaped for the reader's attention, not for completeness-first:
it opens with the few decision points that genuinely need the user's
judgement and how big the ask is, with the full change collapsed below. A
page that opens on the whole diff invites a cursory skim of everything
instead of a real read of what matters — the layout does the triage so the
reader doesn't have to.

`scripts/signoff-page.sh` is the single front door: it composes the page,
publishes or edits it, and watches for the verdict. Drive the sign-off page
only through it — not the wrack client by hand, and not the wrack-pages
skill, even though that skill owns the general Wrack page surface: the watch
depends on the exact form anatomy the script composes, and a page built any
other way breaks it. (The wrack client's own CLI contract is the wrack-pages
skill's to document; re-check there if the script fails against an intact
connection.)

## Publish the Read

Build the content from git, so the read shows what will be committed:

- **Update** (any sign-off where a prior committed contract exists — an
  update conversation, a hold, a release, a withdrawal, a correction): the
  object is the diff. New `held/` and `shed/` files are untracked until the
  closing commit and a plain `git diff HEAD` omits them, so register intent
  first, then diff — from the annexe root:

  ```sh
  git add -N README.md VERIFICATION.md held/ shed/
  git diff HEAD -- README.md VERIFICATION.md held/ shed/ > /tmp/signoff.diff
  ```

  Check the diff before publishing: it is wrong if it omits any file the
  closing commit will touch. Pass it with `--diff`, and pass the draft README
  with `--readme` so the page offers the full file behind an expander (skip
  `--readme` only when the diff already shows the whole README).
- **Initial capture** (no committed README yet): the object is the file
  itself. Pass the draft with `--document`. Any sign-off whose object is a
  single new file can be published whole the same way.

**Write the decision points before publishing.** Distil the round into the
few assembly-level items that most deserve the user's attention in the final
read — a Decision settled or retired this session, a proposed choice they
merely accepted, anything where a wrong call costs something real — ranked by
stakes in a blank-line-separated file: each item a plain-language headline,
then two or three sentences ending with the cost of a wrong call, with no
blank line inside an item (a blank line starts the next card). The cards
re-present what the conversation already settled; nothing reaches the page
unsettled — in particular a cap, budget, or ceiling must already have its own
conversational answer (the caps rule in `SKILL.md`) before it may appear as a
card. Leave out minor items the handshake fully closed — with three exceptions
that are always cards, however small they read: a claim the preservation
review's gains direction surfaced as an unsettled
addition and the round then settled — it entered the draft without a
conversational moment, so the page is where the
user actually sees it; each want the commitment audit's exclusion diff
surfaced that the round settled as a deliberate exclusion, so the user signs
over what was left out as well as what was written; and each hollow verdict
the round accepted as a residual — sharpening it would demand
over-specification the surprise test forbids — carrying the verdict's cheap
reading and the acceptance reason, so the signature is what closes it. Write each such card with
what the decision changes and the cost of a wrong call, like any other. Pass the file with
`--decisions-file`; omit the flag only for a genuinely decision-free round
(a pure wording tidy), where the page falls back to the artifact-first
layout.

**Tags are final before publishing.** Every Decision's examined/provisional
tag is set by what actually happened in conversation — compared or merely
named — before the page goes up. The verdict never changes the draft: after
any approval the only thing that varies is what the commit message records,
so the user always signs exactly the bytes that land.

```sh
scripts/signoff-page.sh publish --diff /tmp/signoff.diff --readme README.md \
  --decisions-file /tmp/signoff-decisions.txt \
  --title "<project> — commission sign-off"
```

The JSON result carries `page_id`, `url`, and `responses_seen`. Hand the URL
to the user together with the sign-off report **before** starting the watch —
publish and watch are separate invocations, never chained in one command, or
the user waits unknowing while the session blocks on a page they were never
given. Keep the `page_id` and `responses_seen` for the watch, revision
rounds, and the revoke.

`--intro-file` prepends round-specific context — a gate the user chose to
skip or degrade, a reinstatement folded in from preservation — above the
page's standard explanatory paragraph, which is always shown. The full
sign-off report still belongs in chat with the link.

## Watch for the Verdict

There is nothing else to do while a sign-off is open, so the watch runs in
the foreground as the turn's closing act:

```sh
scripts/signoff-page.sh watch <page-id> --seen <responses_seen>
```

`--seen` takes the `responses_seen` from the publish or edit that opened this
round; it is how the watch tells this round's click from an earlier one. The
watch exits 0 with `{"verdict", "comments", "submitted_at_unix"}` the moment
the user answers.

Shell tools cap a command's runtime, so grant the watch the largest window
the tool allows and keep `--timeout-secs` just under it (the default, 570,
fits a 600-second cap). The script's own deadline exits 3 with a timeout
verdict; a watch killed by the tool's cap, or dying on an error mid-wait,
means the same thing: no answer yet. None of these is a verdict — re-invoke
the watch and keep waiting. Only the user ends a sign-off — by clicking, by
answering in chat (which counts exactly as a click), or by saying to stop
waiting. Ending a *turn* does not end the sign-off: after a few empty
watches, suspect the link never reached the user — re-state the URL (the
publish receipt in this session still holds it) as the final message of a
turn and stop watching; the page keeps collecting the verdict, so check
responses before anything else when the user next speaks.

## Act on the Verdict

Read the comments before acting on the button — the comments are the user's
words and outrank it:

- **`approve`, comments empty or advisory** → the sign-off yes. Land the
  closing commit and push as the Sign-off doctrine directs, approval recorded
  in the message.
- **`approve-skim`, comments empty or advisory** → also the sign-off yes —
  land and push the same unchanged draft, noting the skim in the closing
  commit message. The tags already reflect the conversation (set before
  publishing), and a skimmed approval never upgrades one: it is consent, not
  comparison, so nothing it covered gains the reopen-shield examined status
  carries. Accept the verdict without re-asking — an honest light read beats
  a pretended full one. Comments carrying an exact correction or a
  substantive change are classified exactly as under `approve`: the button
  says how the user read, the comments say what they want, and the comment
  branches above apply regardless of which approval button carried them.
- **`approve`, comments supplying an exact correction verbatim** — replacement
  text that is literally the user's own pen — → apply it and land with the
  approval. The moment applying a comment requires composing words, however
  few, it is not this case: treat it as `revise`.
- **`approve`, comments asking for substantive change** → a revision round
  wearing the wrong button. Treat it as `revise`.
- **`revise`** → the comments come back into the conversation as user input:
  work them through the normal handshake, revise the draft, **rebuild the
  diff from git** (the same intent-to-add and diff commands as above — an
  edit that reuses the pre-revision diff file shows the user a page without
  the very change they asked for), then run the next round on the *same
  page*:

  ```sh
  scripts/signoff-page.sh edit <page-id> --diff /tmp/signoff.diff --readme README.md \
    --decisions-file /tmp/signoff-decisions.txt
  ```

  `edit` rebuilds the whole page from its flags, so repeat `--decisions-file`
  (updated for the revision) and any `--intro-file` — omitting them silently
  drops those sections from the revised page. Watch again with the fresh
  `responses_seen` it prints. Editing in place
  keeps the URL the user already holds — a one-line "revised, same link" in
  chat is enough.

Revoke the page whenever this round's page will no longer be watched: the
verdict acted on, the user stopping the wait, or the session falling back to
the in-chat read after a successful publish. A session resuming an
interrupted conversation that finds a live sign-off page from the old round
revokes it before publishing anew.

```sh
scripts/signoff-page.sh revoke <page-id>
```

## When Pages Are Unavailable

Fall back to the in-chat read — the user reads the diff or file in the
conversation and answers there, exactly the read the doctrine describes —
and say the page could not be published, so the fallback reads as a decision
rather than an oversight. Lead the in-chat read with the same decision
points the page would have carried. All three verdicts exist here too: when
the user's yes doesn't say how they read, and Decisions were settled this
round, ask whether it was a full read or a skim, and record a skim exactly
as the `approve-skim` handling above. The triggers, by how the script fails:

- **exit 4** — no wrack client, no state directory, or the deployment
  unreachable: pages are unavailable; fall back. Never reconstruct a client
  connection from found credentials; connecting a machine is an operator act.
- **exit 1 on a watch** — the sign-off is still open; re-invoke the watch.
- **exit 1 on a publish or edit** — read stderr: fix a usage error and
  retry; retry a transient failure once; only then fall back, with the
  reason reported.
