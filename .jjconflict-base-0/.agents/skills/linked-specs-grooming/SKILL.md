---
name: linked-specs-grooming
description: Use when asked to groom, trim, prune, or reduce an accumulated Linked Specs corpus by sampling records and removing low-value, redundant, or overlapping content.
user-invocable: true
editor-notes: The sampling command intentionally assumes that `shuf` is available.
---

# Grooming Linked Specs

Read `linked-specs`, `linked-specs-updating`, and `linked-specs-review` first.
The goal is a smaller, lighter corpus that preserves important durable
knowledge. Prefer removing documentation weight over polishing records that
should not exist.

## Select the sample

Use the requested positive record count, defaulting to 10. Honor any requested
component or directory scope; otherwise use the project root. Unless the
requester names specific records, run from that scope and capture a random
sample before reading any candidates:

```sh
N=10 # replace when the requester specifies another count
find . -type f -path '*/specs/*.md' | shuf | head -n "$N"
```

Capture the output as the primary sample. Do not resample based on apparent
quality. When fewer than `N` records exist, groom all of them.

The selected records are the primary sample. Read linked records, references,
nearby implementation, tests, and local documentation as needed to judge them;
these supporting records do not count against the sample.

For each sample, search the corpus using its ID, heading, slug terms, and a few
distinctive subject phrases. Inspect likely matches for unlinked duplication
without reading every record indiscriminately.

## Evaluate value and overlap

For each selected record, ask:

- Would deleting it lose important, durable knowledge that should constrain or
  inform future implementation or review?
- Does it still meet its record type's threshold, or is it minor, local,
  mechanically evident, generic guidance, history, planning, or an
  implementation inventory?
- Which sections are incidental, repetitive, stale, or more detailed than
  future work requires?
- Does it restate code, types, tests, CLI help, configuration, comments, or
  other executable facts that a reader can discover directly?
- Does another record describe the same subject? Which record should own each
  fact, and does each record still have an independent purpose?
- Can repeated context be replaced by one useful link? If one record has no
  independent purpose, can its unique high-value content be merged into the
  canonical record and the duplicate removed?
- Are links, status sections, examples, rationale, and metadata still useful
  and current?
- For `GATE-*`, does the record block agents from changing its governing
  constraint, state that constraint precisely, and explain why the gate exists
  and what the user is trying to accomplish without imposing additional
  constraints or describing or tracking implementation?
- For `SPEC-*`, is there a specific non-local behavioral contract with no
  coherent local owner, does one honest and concrete justification sentence
  identify the distributed implementation areas and explain that lack of an
  owner, and has copied code been replaced by identifiers?

An unreferenced record is not automatically useless, and a referenced record is
not automatically valuable. Judge the knowledge itself and the cost of keeping
it current.

## Groom the corpus

Apply the smallest corpus-preserving outcome that fits:

1. **Delete** a record that contains no qualifying durable knowledge.
2. **Merge** overlapping records when one canonical record can preserve their
   useful unique content; remove the redundant record.
3. **Trim** low-value sections and duplicated explanations.
4. **Interlink** records only when each retains an independent purpose and the
   link can replace repeated context.
5. **Keep** an already concise, justified record unchanged.

Prefer deletion and consolidation over splitting. Split only when multiple
independently useful subjects genuinely need separate records and the result
reduces future reading and maintenance cost.

A grooming request authorizes removal of demonstrably redundant, evident,
incidental, or non-qualifying documentation. It does not authorize changing
intended behavior, architecture, any gate, or external
requirements. Escalate ambiguous semantic changes under the normal Linked
Specs authority rules. When deleting or merging records, update all references;
do not leave tombstones or compatibility indexes by default.

Relocate content to ordinary documentation only when it remains genuinely
useful there. Do not modify source code, source comments, API documentation
embedded in source, or tests; report a separate follow-up when useful local
knowledge belongs there. Do not preserve trivia merely by moving it.

Grooming never authorizes a gate operation. Apply the gate lifecycle rules in
`linked-specs-updating` when the user separately and explicitly requested an
operation on a sampled gate. Otherwise report any defect and retain the gate
unchanged. Moving its constraint to another record or durable owner does not
authorize changing or removing the gate.

## Review ownership

Grooming is a documentation-only review workflow and does not require a
separate standard code review. The grooming agent is the reviewer and performs
the final Linked Specs validation itself. If source or test changes become
necessary, keep them separate and apply the project's normal code-review
requirements.

## Verify and report

Use the `linked-specs-review` workflow and criteria as a final validation pass
over changed records and affected links; its review-only authority and response
format do not replace this editing workflow. Search for removed IDs and verify
links resolve. Confirm that retained architecture, requirement, and functional
records describe the current system, and that gates accurately preserve
their governing constraints and explain their purposes. Report a gate defect
instead of correcting it unless the gate lifecycle rules authorize the required
operation.

Report:

- all sampled IDs,
- each changed, deleted, or merged ID with its outcome and concise reason,
- each supporting record changed outside the primary sample,
- verification results and blockers requiring a decision, and
- net corpus record and word-count changes.

Assign each sampled record exactly one outcome: deleted, merged into another
record, trimmed (including interlink-only edits), or unchanged. Report counts
for these mutually exclusive outcomes. This replaces the
`linked-specs-review` response format; do not explain unchanged records unless
asked.
