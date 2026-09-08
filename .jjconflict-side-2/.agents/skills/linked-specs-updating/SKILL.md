---
name: linked-specs-updating
description: Use whenever creating, modifying, moving, renaming, or deleting Linked Specs records.
user-invocable: true
---

# Updating Linked Specs

This skill extends `linked-specs` with conventions for keeping records concise, current, and useful to future implementation and review.

## Location and scope

Store records in a `specs/` directory within the directory whose code they govern. Prefer a project-root `specs/` for project-wide knowledge and a package-root `specs/` for package-local knowledge. Use deeper or otherwise placed directories only for a concrete locality or ownership benefit.

A record's natural scope is the parent of its `specs/` directory and that parent directory's descendants. Link across scopes when components interact or wider records constrain local ones. Do not centralize local knowledge merely to put all records together.

Do not create index files. Records should be discoverable through search, links, and references from code.

## IDs and filenames

Use `<TYPE>-<short-slug>.md`, with an uppercase type and concise lowercase kebab-case slug:

```text
ARCH-runtime.md
GATE-nonblocking-network-operations.md
REQ-export-retention.md
SPEC-session-recovery.md
```

`ARCH`, `GATE`, `REQ`, and `SPEC` are the standard types. For a non-standard type, follow the applicable additional skill as well as the shared conventions here.

The filename stem is the record ID. It must be unambiguous within the repository, so search before choosing one and add a component name when needed.

Avoid cosmetic renames. For a useful rename, rename the file, update every reference, and search for the old ID. Add `Previously known as: <old-id>` only when that history still matters. Do not retain redirect or tombstone files by default; version-control history is sufficient.

## Record types

### `ARCH-*`: architecture

Describe current responsibilities, boundaries, component relationships, dependency direction, ownership, flows, interfaces, trust boundaries, and invariants.

Name the default overview `ARCH-<project-or-component-name>`. Keep it focused on the most important high-level topology so readers can orient quickly; it is not an inventory of implementation details. Add focused records only for substantial, independently referable component or subsystem architecture that readers can load when needed. Keep ordinary rationale in the appropriate local documentation. Establish `GATE-*` only under the gate lifecycle and authorization rules below.

### `GATE-*`: user-requested change gates

Preserve the smallest statement of a major governing constraint that agents must not change or reverse without returning to the user. A qualifying constraint affects future work, could reasonably be changed, and is important enough that agent-initiated reversal must be blocked. A gate states what is fixed and what the user is trying to accomplish; it does not describe how the constraint is implemented or managed.

The `GATE-*` type itself prohibits agents from changing or reversing the governing constraint on their own. Do not repeat that prohibition inside each record.

Start the record's substantive content with `## Gate` and state only the governing constraint, directly and precisely. Follow it with `## Justification`, explaining why the gate exists and what the user is trying to accomplish. The justification is informative context: it does not independently impose or expand the normative constraint in `## Gate`. Escalate ambiguity in the normative constraint instead of treating the justification as an additional constraint.

### Gate lifecycle and authorization

Every gate operation requires a user request that explicitly identifies the operation and, for an existing gate, the named gate:

- **Establish:** the user must explicitly ask to establish the gate and state its exact semantic constraint.
- **Amend or replace:** the user must explicitly ask to amend or replace the named gate and state the exact new semantic constraint.
- **Revoke or delete:** the user must explicitly ask to revoke or delete the named gate. No replacement constraint is required.
- **Rename or move:** the user must explicitly ask to rename or move the named gate. Preserve its constraint and justification unless the request separately authorizes changing them.

Treat a merge, split, demotion, supersession, trim, or other compound operation as the corresponding establishment, amendment, revocation, deletion, rename, or move of each affected gate. Require the authorization and exact resulting semantics applicable to every constituent operation. Agents may refine wording but must not infer or broaden a requested constraint.

Nothing else authorizes a gate operation: not implementation or documentation work, research, planning, a recommendation, a conflicting task, the user's selection among options or acceptance of other work, an agent's judgment or request, implementation behavior, or an incidental mention of the gate. Even editorial or mechanical corrections require an explicit user request for that operation on the named gate.

The constraint must still be important beyond the implementing change, and the record must remain useful independently of it. An explicit gate request does not make a minor choice major. Keep candidates, inferred constraints, prospective choices, options still being evaluated, and recommendations in the planning system or appropriate non-gate documentation. Do not elevate accidental or harmful behavior into a gate.

Do not edit a gate to describe a proposed replacement. Implementation drift never authorizes synchronizing a gate. When implementation diverges, keep the gate unchanged and escalate.

### `REQ-*`: external requirements

Record an external obligation, its source or authority, strength and flexibility, justification, consequences, and relevant constraints, acceptance conditions, or exceptions.

Use clear normative language without presenting preferences as mandates. Explain the underlying need well enough to identify contradictions, obsolete assumptions, disproportionate cost, or better solutions. Internal implementation choices belong in appropriate local documentation or, when they meet the record thresholds, `ARCH-*` or `SPEC-*`.

### `SPEC-*`: functional specifications

Create a functional specification only for a non-local behavioral contract whose implementation is necessarily distributed and which no single implementation artifact can own coherently. When the implementation is reasonably localized, keep its documentation beside it instead.

Every `SPEC-*` must include a `## Record justification` section after any `## Status` section and before the functional description. In one sentence, identify the distributed implementation areas and explain why none is a coherent local owner. If that cannot be stated honestly and concretely in one sentence, do not create the record; do not manufacture a justification to retain a desired document.

State only the non-local contract and invariants that the identified local artifacts cannot own. Omit behavior already clear from APIs, types, tests, CLI help, configuration documentation, or nearby comments. Do not restate source files or write an implementation walkthrough.

`SPEC-*` records must not contain source code or implementation excerpts.
Refer to code only by stable identifiers, such as module, type, function,
command, or configuration-key names.

### Alternatives in architecture and specifications

An `ARCH-*` or `SPEC-*` record may include an optional `## Alternatives`
section after its substantive description. Use it to document alternative
designs that were considered, why the current design was chosen over them,
and whether they remain viable. Most entries describe rejected alternatives
and the reasoning behind the current choice. Preserve this context when it
will help future implementation or review understand the solution space
without reconstructing it.

Keep the section selective and current. Do not inventory every possibility,
narrate the decision process, turn alternatives into additional constraints,
or use the section as an implementation plan. Describing a still-viable
alternative does not propose or authorize changing the recorded architecture
or contract. Omit the section when the considered alternatives add no durable
value.

## Record shape and links

Start with the ID and a concise title:

```md
# GATE-nonblocking-network-operations: Nonblocking network operations

## Gate

Network operations must not block executor threads.

## Justification

The user wants responsive shutdown; blocking executor threads would violate [REQ-responsive-shutdown](REQ-responsive-shutdown.md).

```

Use plain prose and only as much structure as needed. Metadata is type-specific, not universal.

Prefer Markdown links with the target ID as link text. Describe relationships accurately, for example `depends on`, `refines`, `implements`, `constrained by`, or `supersedes`. Add links that improve navigation or explain impact. Do not add relation sections mechanically; require reciprocal links only for the migration pairs below.

## Gradual changes and status

When agreed architecture, requirements, or functionality cannot be implemented atomically, add an optional `## Status` section to the applicable `ARCH-*`, `REQ-*`, or `SPEC-*` record so the record set still describes the codebase truthfully. Omit the section when the code is believed to be fully in sync with the record.

Place `## Status` immediately after the heading and required leading type-specific metadata, before substantive text. Status records implementation alignment, not authority; establish the requested end state and staged transition independently, under the record type's normal rules and the project's decision process.

Do not put `## Status` in `GATE-*`. Adoption, rollout, migration, progress, and current implementation coverage are not part of a gate. Track execution in the issue or planning system and describe independently important current structure or behavior in `ARCH-*` or `SPEC-*`.

The section must concisely identify:

- the affected area and what the implementation currently does
- which parts of the record already apply and which do not
- justification, when it is not evident from the transition
- the intended resolution or transition, when known

Keep it as a current-state summary, not a progress log or task checklist. Link to the project's issue or planning system for detailed execution work.

An old and new status-bearing record may coexist while different parts of the code remain governed by each. Give both records a `## Status` section, link them to each other, and state precisely which scopes or behaviors each still describes. Use accurate relationship wording such as `partially supersedes`, `partially superseded by`, or `transitions to`; do not claim complete supersession prematurely.

A gate changes under the gate lifecycle rules, not when rollout finishes. Do not retain or qualify an old `GATE-*` to track implementation progress. Let version-control history preserve a replaced or revoked gate, and describe independently important transitional structure or behavior in status-bearing records.

When an old record is retained solely to describe implementation that has not yet migrated to its successor, append `(obsolete)` to its heading title, for example `# SPEC-old-flow: Old flow (obsolete)`. Its status must identify the still-affected implementation. Do not label a record obsolete if it remains the agreed specification for an independent scope; narrow or split the record instead.

Once migration is complete, remove migration status sections from every record, and remove the superseded record unless it still describes independently current knowledge. Narrow or rewrite any retained predecessor and replace transitional relationship wording. Update references as part of the same change; version-control history preserves the obsolete record.

## Content boundaries

Linked Specs are not a planning system or general-purpose descriptive documentation. Create a record only for durable information substantial enough to affect future implementation, maintenance, or review. Keep one cohesive, independently referable subject per record.

There is no target record length or completeness threshold. Prefer omission over splitting. Splitting does not make incidental detail appropriate.

Prefer comments, API documentation, tests, or ordinary project documentation for local, mechanically evident, executable, or minor information.

Apply the gate lifecycle and authorization rules to every edit, deletion, merge, replacement, demotion, rename, move, or trim. Moving a constraint elsewhere does not authorize an operation on its gate.

Do not add routine lifecycle metadata, and keep undecided future proposals in planning or issue systems.

Outside the deliberate gradual-change process above, correct editorial or mechanical errors, and synchronize exact semantic end states already explicitly requested by the requester in non-gate records, in every affected artifact you are authorized to edit; request synchronization of every affected artifact you are not authorized to edit. That request concerns the intended result, not the wording of the record. If code and a record disagree and the correct artifact is not immediately clear, treat the conflict as a substantive change under the rule below.

When a governing record blocks requested implementation or appears to require a substantive change, stop the affected work and promptly escalate to the task requester. Describe the conflict concisely and state that resolving it likely requires a user decision. Do not start additional review, research, planning, alternative exploration, or drafting unless, after receiving the escalation, the requester explicitly asks for specific further work; investigate only enough to identify and explain the conflict. Never use `## Status` to bless accidental drift.

Architectural changes should support the change's main goal. An unnecessary or out-of-scope architectural change requires an explicit user or maintainer request.

When strong technical, product, safety, or cost reasons undermine a `GATE-*` or `REQ-*`, escalate the conflict under the rule above instead of silently rewriting or disregarding it.

## Existing documentation

Linked Specs gives no special meaning to `ARCHITECTURE.md`, `design.md`, or similar files. Respect their project-defined purpose. Migrate and remove them only when they duplicate Linked Specs and are truly obsolete and unreferenced. Do not create compatibility indexes. Create migration shims only when requested.
