# [M] A Firefox player on ARM returns to live after the replacement section

## Goal

On the Nightly's runner class (`ubuntu-24.04-arm`), a full `just test media
--media-engines firefox` run passes `lifecycle player returns`, the reattach
after the lifecycle pages idle through the `replacement` section, at the root
of the regression [moq-dev/moq#9508](https://github.com/moq-dev/moq/pull/9508)
brought, without loosening its bound.

## Plan

Found while validating [moq-dev/moq#9530](https://github.com/moq-dev/moq/pull/9530),
bisected on ARM through a temporary `pull_request` workflow (3 full Firefox
runs per job, 4 jobs per variant):

- `main` at `35e6841f4f`: 24 of 24 runs fail only at `lifecycle player returns`
  ("held a frame 1800ms behind the fixture 2000ms after the first frame,
  against a 1500ms bound"), with and without #9530's fix
  ([run 45361439037](https://github.com/moq-dev/moq/actions/runs/45361439037),
  [run 45367640157](https://github.com/moq-dev/moq/actions/runs/45367640157)).
- With #9508 reverted: 11 of 12 pass; the 12th failed `sampling: audio
  channel-count change` only. With [moq-dev/moq#9422](https://github.com/moq-dev/moq/pull/9422)
  reverted instead: 12 of 12 fail
  ([run 45371229666](https://github.com/moq-dev/moq/actions/runs/45371229666)).
- [moq-dev/moq#9524](https://github.com/moq-dev/moq/pull/9524)'s runs at
  `667edefed5`, based before #9508 and #9422 landed, passed 12 of 12.
- At the failure the host is saturated: `moq-relay` at 80 to 110% CPU,
  `chrome:renderer` (the fixture) above 100%, the Firefox player near 100%,
  under 10% idle; the player logs `skipping slow groups` on audio, one group
  at a time, and its page answers no long-animation-frame query within 2 s.
  It passed in a local x86 run.

#9508 made a held track info lookup count as demand (`Demand.wanted`), so an
on-demand publisher runs while a TRACK is held. Find which held lookup keeps
which track wanted through the replacement section (the JS fixture's
`onDemand: "retain"` tracks, a relay's held TRACK to the fixture, or a lookup
whose hold never lets go), and what the relay then spends its CPU on at the
return; fix that at the root, in JS and Rust alike. Measure the return on ARM
before and after with a temporary workflow, as #9530 did.

Public API: none expected. Wire: none expected.
