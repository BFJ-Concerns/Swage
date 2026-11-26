# Accuracy review

A project's documentation has just been written or updated, and before it is
published each of its claims needs checking against the code. You are doing
that check. The writer will correct or remove every claim you do not confirm,
so a claim you could not settle is as useful to them as one you disproved,
and a guess in either direction is not.

Below this brief is a list of claims. Each row gives the claim, the source
the writer attributed it to, the documentation file and place where it now
appears, and what it is to be checked against. You have the repository. You
have not been given the writer's reasoning, and the check is worth more
without it.

## What to do with each claim

Decide whether the claim is true as written.

- A row checked against **the code**: read the implementation and say
  whether it supports the claim. The cited source is where to start and is
  not the answer. A citation that exists but does not say what the claim says
  leaves the claim unsupported.
- A row checked against **another documentation page** (a page for people
  that restates a page for agents): say whether the two state the same
  facts, with nothing added in the retelling. If the page it restates is not
  among the pages changed in this job, also check that page's statement
  against the code. A faithful copy of a stale page is false.

Give each claim one verdict:

- **Confirmed**: the code supports the claim as written.
- **Falsified**: the code contradicts it. Cite what contradicts it, as a
  file and line or the output of a command.
- **Unverifiable**: you could not establish it either way. Say what you
  checked and what was missing.

Reference rows are where errors concentrate: flags, defaults, types and
error codes, whose exact spelling and value should match the parser, schema
or enum; and with them version numbers, command lines, and statements about
what happens in an edge case.

To check a command, read-only invocations such as `--help` and `--version`
are fine. Do not run builds, test suites or anything that changes state.

## What else to report

While reading the changed documentation, report with evidence anything the
list missed:

- a statement describing behaviour that is not implemented;
- an exact value or procedure stated in more than one file. A page for
  people that restates its agent counterpart, and a generated index, are
  intended; for those, report only a difference between the copies;
- a link or cross-reference that does not resolve;
- a statement true only on one machine or deployment (a service unit, a
  deployment path, host-specific setup) that is written as though true
  everywhere;
- material about how the project is developed (contribution setup, release
  mechanics, internal design or decision records). Building from source and
  operating a running instance are about using the product and belong;
- a secret, a credential, a private hostname or a path that exists only on
  one machine.

## Limits

Report only. Do not edit the documentation or any other file in the
repository; corrections are the writer's. Check every claim on the list. If
the list is a sample, the text above it says so; state the coverage you had
and draw no conclusion about claims you did not check.

## What to send back

One line for each claim: the verdict, then the evidence or what you checked.
After those, the other findings. Last, a short paragraph giving the number
confirmed, falsified and unverifiable.
