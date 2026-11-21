# [L] A route switch caps the old subscription at one boundary

## Goal

When a front moves a track to another route of the same instance, the reader
sees one subscription split at one boundary, chosen at the switch. The
boundary is the largest group any copy holds, past the frames held
contiguously in it. The new subscription starts there. The old one is capped
at that group on the wire at once and drains what lies below it within the
budget. Each frame is delivered once, from whichever copy brings it first.
Rust and `@moq/net` match.

## Plan

Found by the 2026-11-20 audit of the subscription rules before the lite-07
cut. Today:

- Rust `Reader::sync` (`rs/moq-net/src/model/resume.rs`) resumes from the
  newest group handed to the reader (`held_at`, `resume_at`), not the largest
  any copy holds. JS `sync` (`js/net/src/resume.ts`) asks that group from
  frame 0, which [js-route-resume](/quest/m4/js-route-resume.md) owns.
- A switch only marks the old copy replaced. Its bound is chosen later, by
  `observe` when the replacement hands out its first group, or by
  `bound_at_end` (JS `#observe`, `#boundAtEnd`). It reaches the wire as
  SUBSCRIBE_UPDATE only if the copy's aggregate demand narrows, so until then
  both routes send every new group.

Decided 2026-11-20 (maintainer, /quest-plan): ✅ cap the old at the switch, by
group (rejected: keep today's late bound; cap by frame now, which
[switch-frame-cap](/quest/m1/switch-frame-cap.md) adds on top of this).

- At the switch, pick the boundary group G and frame N. The replacement starts
  at Position (G, N). The old copy's ranges end at G, so it finishes G and
  sends nothing newer. That goes out as SUBSCRIBE_UPDATE right away.
- The old copy keeps draining every group below G within the budget, as
  `let_go` already allows. A group it can't deliver is asked of the new route
  (`backfill_gaps`).
- Once the replacement delivers past the old copy's position in G, stop only
  G's stream on the old copy, which also ends a group that never finishes.
- Run the resume and failover models (`/verify-resume`, `/verify-failover`)
  after updating them, since they encode today's bound. Teach the resume
  oracles (`resume_model.rs`, `resume.model.test.ts`) the fixed boundary.

Test: a switch while the old route still delivers sends no group above G from
the old route, loses no group below G, and delivers G's frames once, in both
languages.

Public API: none expected. Wire: none.
