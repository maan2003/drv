This project uses the Linked Specs convention; consult the `linked-specs`
skill before working with specs or governed code.

## Engineering judgment and coordination

The primary user-facing coordinator is the judgment owner for tricky calls
because it maintains the project owner's end-to-end goals and tradeoffs.
Engineers should bring it unresolved cross-owner disagreements, ambiguous
architectural or safety tradeoffs, and choices that optimize a subsystem at the
expense of the overall product. Include the evidence, alternatives, and a
recommendation. The coordinator resolves calls within the agreed direction and
returns to the user when a decision requires changing that direction or granting
new authority. An advisor supplies technical analysis, not product authority.

Engineers retain agency over routine implementation, testing, peer coordination,
and integration within their ownership. The coordinator is not a mandatory
reviewer, message relay, or approval gate for every change. Record delivery
evidence in jj commit descriptions; advance local master with
`jj bookmark move master --to <tested-revision>`, never `-B`. Reconcile and
retest when master has advanced rather than bypassing the ancestry check.

Use [ARCH-drv](specs/ARCH-drv.md) for the durable product direction: a secure
Linux laptop built from native Rust userspace stacks with strong process
sandboxing; MT7921 production readiness and Redwood bring-up toward proved
Internet connectivity are distinct goals.
