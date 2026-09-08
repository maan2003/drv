---
name: linked-specs-claims
description: Use when reading, creating, or changing a Linked Specs CLAIM-* record, or when work must preserve a documented load-bearing project property. For proving or independently verifying a claim, also use linked-specs-claims-verification.
user-invocable: true
---

# Linked Specs Claims

Read `linked-specs` first. Claims add concise, load-bearing properties that the code in scope claims to hold. Ordinary work uses the property as context to uphold; use `linked-specs-claims-verification` only for explicit proof or verification.

## When to create a claim

Create a claim only when the requester explicitly asks for one; do not infer that request from implementation work, a review finding, or an apparently valuable property. It must describe an important property whose failure would materially harm the project and whose concise statement will help future work preserve it. Do not claim a desired feature, plan, implementation detail, generic quality goal, or fact already evident from a local type, API, test, or comment.

Claims are non-gate records. If code and a claim disagree, synchronize them only to an exact semantic end state the requester explicitly requested; otherwise preserve the discrepancy, document a falsification when appropriate, and escalate.

## Claim records

Store claims in the applicable `specs/` directory as repository-unique `CLAIM-<short-slug>.md` records.

A claim record contains only a short, direct property statement. Do not add metadata, rationale, proof, verdict, implementation references, or status sections.

Use the normal Linked Specs leading heading followed by the claim:

```md
# CLAIM-single-writer: Persistent job state has one writer

Only `JobCoordinator` writes persistent job state; all other components submit transitions to it.
```

State a durable, falsifiable property precisely enough to judge whether a change preserves it. Include material qualifiers, such as the protected state, boundary, or concurrency condition, but not an implementation explanation.

## Companion evidence

A sibling `CLAIM-<short-slug>/` directory holds checked-in proof and verification documentation for that claim. Create it when a task, project convention, or the claim's risk requires explicit verification; never put evidence in the claim record.

When work may affect a claim, find it through the applicable `specs/` directories, links, IDs, and targeted search. Read the property before designing or reviewing the change. Uphold it, and use `linked-specs-claims-verification` when the task requires evidence that it still holds.
