# [L] WHEP adaptive rendition switching

## Goal

A WHEP viewer of a multi-rendition broadcast is switched between video
renditions from its own congestion feedback, the way `@moq/watch` switches
over MoQ, so a weak link gets SD instead of stalling on HD.

## Plan

Open questions: whether to drive switching from str0m's bandwidth estimate
(TWCC) or from loss and REMB, how to switch without a keyframe gap (subscribe
the new rendition and splice at its next group, as the JS decoder does), and
whether offering the renditions as WebRTC simulcast layers (RIDs) buys
anything for a receive-only browser. Keep the
[best-rendition pick](/quest/m1/egress-rendition-pick.md) as the starting
rendition.

## Required

- [Egress rendition pick](/quest/m1/egress-rendition-pick.md) - the shared ranking this starts from
