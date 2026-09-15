# The Human Register

Patterns that make documentation read as generated rather than written. Read
this before drafting or materially revising human-facing prose — README
narrative, guides, rationale, runbooks, whether written fresh or added
inside a refresh patch — and hold the patterns out from the start;
retrofitting them out costs more than never writing them.

Neutral and plain is the correct voice for documentation: no injected opinions,
no first-person asides in reference material, no personality sprinkled on. What
follows is not a push toward casual prose — it is the removal of the specific
tells that generated text carries even at a neutral register.

## Content patterns

**Significance inflation.** Watch for: stands as, serves as, is a testament to,
plays a vital/crucial/pivotal role, underscores, highlights the importance of,
reflects broader, marking a shift, key turning point. Documentation states what a
thing is and does; it does not argue for the thing's importance. If a sentence's
only work is to assert significance, cut it.

**Promotional vocabulary.** Watch for: seamless, powerful, robust, blazingly
fast, comprehensive, cutting-edge, effortless, rich, vibrant, boasts, showcases.
A README that says "handles X, Y, and Z" outranks one that says "a powerful,
comprehensive solution for X". Claims of speed or robustness stay only when a
measurement or mechanism backs them, stated as that measurement or mechanism.

**Tacked-on "-ing" analysis.** Watch for sentence tails like ", ensuring
consistency", ", enhancing performance", ", allowing for flexibility". These
assert a benefit without saying how it arises. Either state the mechanism as its
own sentence or cut the tail.

**Copula avoidance.** "The gallery features four topics and boasts 3,000 square
feet" → "The gallery has four topics and 3,000 square feet". Plain "is", "are",
and "has" are correct; elaborate substitutes (serves as, represents, offers,
provides a way to) are the tell.

**AI-frequent vocabulary.** Watch for: delve, leverage (as a verb for "use"),
utilize, facilitate, streamline, intricate, pivotal, crucial, foster, garner,
landscape (abstract), tapestry, interplay, showcase, underscore. Each has a
plainer word: use, help, simplify, detailed, central, important.

**Manufactured triads.** Lists of exactly three abstract benefits ("innovation,
inspiration, and insights") are a rhythm reflex, not information. A list has as
many members as the subject genuinely has.

**Negative parallelisms.** "It's not just an installer, it's a complete
onboarding experience" — rewrite as the plain claim: say what it does.

**Vague authority.** "Industry best practice", "many developers prefer",
"experts recommend" — documentation grounded in this repo cites this repo's
reasons or none.

**Generic conclusions.** Endings like "the future looks bright" or "happy
coding!" carry nothing. End on the last substantive point.

**Filler and stacked hedges.** "In order to", "it is important to note that",
"at this point in time" → "to", (delete), "now". Collapse stacked qualifiers
("could potentially possibly") to the one honest hedge the claim needs.

## Structure patterns

**Inline-header bullet lists.** Bullets shaped `**Label:** sentence restating
the label` are generated scaffolding. Where the items are genuinely parallel
facts, a table or plain list serves; where they are prose, write the paragraph.

**Boldface as emphasis machine.** Bold marks the term a reader scans for — a
command, a filename, a defined term at first use — not every phrase the writer
considers important.

**Heading warm-ups.** A one-line paragraph after a heading that restates the
heading ("## Performance — Speed matters.") is padding; start with the first
real point.

**Signposting.** "Let's dive in", "here's what you need to know", "without
further ado" — delete the announcement and start.

**Diff-anchored writing.** Documentation describes the system as it is, not the
change that produced it: "this function replaces the previous approach, which
was O(n²)" belongs in a commit message or changelog. The doc says what the
function does now and why that design holds. (Changelogs and migration guides
are the exception — they are inherently version-scoped.)

**Title Case Headings.** Use sentence case in headings unless the project's
existing docs already do otherwise.

## What not to sand off

Polish is not a tell. Perfect grammar, formal vocabulary, an occasional
transition word, and genuinely three-membered lists are all fine. Avoid the
specific patterns above and stop there: over-editing plain correct sentences
into fragments is its own defect. When an existing doc's voice is human and
clear, refresh mode preserves it (gate 4) — this reference governs prose that
gets newly written, in write mode or inside a refresh patch.
