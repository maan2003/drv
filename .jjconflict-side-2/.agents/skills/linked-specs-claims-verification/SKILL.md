---
name: linked-specs-claims-verification
description: Use when asked to prove, falsify, independently verify, or re-verify a Linked Specs CLAIM-* property, or when a change affects its proof evidence. Maintains checked-in evidence in the claim's CLAIM-<short-slug>/ directory.
user-invocable: true
---

# Verifying Linked Specs Claims

Read `linked-specs` and `linked-specs-claims` first. This skill adds an explicit evidence workflow for a claim; it does not expand the claim record itself. The claim remains a short property in `CLAIM-<short-slug>.md`. Its sibling `CLAIM-<short-slug>/` directory holds all proof and verification documentation.

A proof argues from the current code that the stated property holds under an explicit model. Verification independently attacks that argument. Both are evidence, not authority: code changes may invalidate them, and a proof never turns a claim into a gate.

## Evidence layout

Use the claim's sibling directory, for example:

```text
specs/
├── CLAIM-single-writer.md
└── CLAIM-single-writer/
    ├── proof.md
    └── verification.md
```

Keep the names above unless a project convention needs additional narrowly named evidence files. Do not create an index. Keep every document specific to its claim, use relative links to code and tests where useful, and check it into version control with the claim or relevant code change.

`proof.md` contains the author's derivation. `verification.md` contains an independent verifier's result. The person or agent who wrote the proof must not be its sole verifier. A task that only needs the claim for implementation context does not need either file.

## Proving a claim

Derive the proof from the current code, tests, configuration, and deployment model; do not reconstruct it from memory. State the exact property copied or linked from the claim record, then record only the evidence needed to establish or limit it:

1. **Scope** — code, configuration, interfaces, data, and execution paths the argument reads.
2. **Model and quantifiers** — the relevant inputs, actors, failure points, concurrency, and exact durable predicate. Enumerate a domain only when it can be checked or regenerated.
3. **Axioms** — each trusted external premise and where the guarantee bottoms out. State library, operating-system, protocol, cryptographic, deployment, or operator assumptions rather than smuggling them into a lemma.
4. **Argument** — short, numbered lemmas from the code and axioms to the property. Cite the relevant source locations, tests, or mechanically regenerated enumerations. Label each lemma by the mechanism that would catch a regression: `type`, `schema`, `test`, `code`, `enum`, `claim`, or `axiom`.
5. **Residuals** — executions deliberately outside the claim's stated quantifiers, with the reason. A counterexample inside the stated property falsifies the proof; do not file it as a residual.
6. **Weakest links** — the least mechanically enforced lemmas and how future work can strengthen them.

Use `claim` only when relying on the exact conclusion of another current, independently verified claim. Link its record and evidence, state the imported conclusion, and treat changes to its claim or evidence scope as a reason to re-verify this proof. Keep claim dependencies acyclic.

Do not manufacture a proof for a property that a type, schema constraint, or focused test already establishes locally unless the proof covers a distinct, load-bearing cross-cutting property. A proof should pay for its maintenance through a credible failure mode, a named residual, or a property whose preservation needs more context than its implementation provides.

## Verifying a proof

An independent verifier checks the argument against the code, not by rereading it sympathetically. Record the verification in `verification.md` with:

- the claim and proof revision or source state checked
- the verifier and date
- code, configuration, tests, and imported claims inspected
- attacks performed: source-level guards, ordering, error and exit paths, lock or transaction boundaries, concurrent interleavings, enumeration completeness, axiom sufficiency, and residual classification
- the result: `pass`, `provisional`, or `falsified`
- every material finding, counterexample, repair, or remaining uncertainty

`pass` means the argument survived this verification at the recorded source state, not that failure is impossible. `provisional` means verification has not completed or lacks needed evidence. `falsified` names the counterexample and remains visible while the discrepancy is resolved. When the claim is retired or narrowed, carry decision-relevant counterexample material into replacement evidence when needed, then remove the retired claim and its evidence directory; do not keep an orphaned historical archive or silently delete evidence needed to resolve the discrepancy.

## Re-verification and change handling

When a change intersects a proof's scope or changes its model, axioms, cited test, imported claim, dependency, or deployment assumption, identify the affected coverage in the same change. Regenerate `enum` evidence, rerun named tests, or reread code-level arguments when needed to describe the changed coverage accurately. If the existing independent verdict no longer covers the source state, change it to `provisional` and record what requires re-verification.

Stale evidence does not by itself block an otherwise authorized code change. Require a fresh independent verification when the task or project policy requires current verified evidence, or before recording a new `pass` verdict. Otherwise report the changed coverage and keep the evidence provisional until verification occurs.

Treat repeated staleness as a signal to strengthen enforcement: replace an enumeration with a lint or test, a code-reading lemma with a focused test, or a runtime convention with a type or schema constraint where practical. Keep the evidence shorter as enforcement becomes mechanical.

If verification finds that the stated claim does not hold, report the counterexample promptly. Do not update the claim merely to match accidental behavior. Synchronize the code and claim only to an exact semantic end state the requester already explicitly requested; otherwise escalate whether to repair code, narrow or remove the claim, or deliberately change the intended property.
