# Prose written for people

This covers the README's narrative, guides, explanations and runbooks,
whether written new or added inside a correction. Prose in an existing
document that is already clear and plainly a person's is left as it is.

## What it should be

Documentation prose is plain and neutral. It says what a thing is and what it
does, in the words someone who knows the product would use to a colleague. It
carries no opinions, no personality, and no first person in reference
material.

- **State the fact and stop.** A sentence either tells the reader something
  about the product or is cut. A benefit is stated as the mechanism that
  produces it, in a sentence of its own, or left out. A claim of speed or
  reliability is the measurement or the mechanism behind it.
- **Use the plain word.** "Is", "has" and "use" are right. A longer word
  standing in for one of them adds nothing.
- **Let the subject set the shape.** A list has as many items as the subject
  has. Prose that is an explanation is written as paragraphs, and facts that
  are parallel go in a table or a plain list.
- **Start with the first real point.** No announcement of what is coming, no
  paragraph restating the heading, and no closing remark after the last
  point.
- **Describe the product as it is.** What changed, and what it replaced,
  belongs in a commit message or a changelog. Changelogs and migration guides
  are the exception, since they are about a version.
- **Give this project's reasons.** Where a choice needs a reason, give the
  one that holds in this repository, or none.
- **Headings in sentence case**, unless the project's existing documents do
  otherwise. Bold marks what a reader scans for: a command, a filename, a
  term where it is defined.

## Reading a draft back

Generated prose has habits that survive a neutral tone. When reading a draft
back, look for these and rewrite the sentence as the plain claim, or cut it
where no claim is left:

- a sentence whose only work is to say that something matters: "plays a
  crucial role", "serves as", "stands as", "underscores";
- praise in place of description: "powerful", "seamless", "robust",
  "comprehensive";
- a tail that asserts a benefit without saying how it arises: ", ensuring
  consistency", ", allowing for flexibility";
- an elaborate substitute for "is" or "has": "features", "boasts", "offers",
  "provides a way to";
- words that turn up far more often in generated text than in documentation
  written by hand: "delve", "leverage", "utilize", "facilitate",
  "streamline", "pivotal", "landscape";
- three abstract nouns in a row where the subject does not have exactly
  three;
- "it's not just X, it's Y";
- an appeal to unnamed authority: "best practice", "many developers
  prefer";
- filler and stacked hedges: "in order to", "it is important to note that",
  "could potentially";
- bullets shaped as a bold label followed by a sentence restating the label.

Correct grammar, formal vocabulary, an occasional connecting word and a list
that really has three members are all fine. Stop at the habits above; a plain
correct sentence edited into fragments is worse than it was.
