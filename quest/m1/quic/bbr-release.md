# [M] Release the BBR correctness fixes

## Goal

Published MoQ consumers receive the seven corrected BBR behaviors through
immutable releases of the noq fork and its adapters. Fixes do not wait for
qmux, stream scheduling, or other unrelated QUIC features.

## Plan

After the fork bootstrap, publish the corrected dependency chain and update
MoQ's manifest and lockfile pins. Record the parent commit, carried patches,
and upstream status. Follow the existing fork packaging rules; no mutable
branch or workspace-only patch may stand in for a release.

Fixes merged in the fork and awaiting release:

- [moq-dev/noq#3](https://github.com/moq-dev/noq/pull/3) - BBR packet identity across QUIC spaces
- [moq-dev/noq#4](https://github.com/moq-dev/noq/pull/4) - each BBR ACK sample completes before the model uses it
- [moq-dev/noq#5](https://github.com/moq-dev/noq/pull/5) - the controller hears of application starvation before the next send
- [moq-dev/noq#6](https://github.com/moq-dev/noq/pull/6) - finished bandwidth probes age the max-bw window once and stop classifying losses as probe feedback
- [moq-dev/noq#7](https://github.com/moq-dev/noq/pull/7) - the first measured RTT replaces the 1ms estimate behind BBR's startup pacing rate
- [moq-dev/noq#8](https://github.com/moq-dev/noq/pull/8) - packets sent during ProbeRTT are application-limited, so its reduced rate cannot lower BBR's bandwidth model
- [moq-dev/noq#9](https://github.com/moq-dev/noq/pull/9) - a spurious loss episode restores the BBR model, window, and probe saved when the episode began

Verify the fork's regression suite and MoQ's default and supported runtime
builds against the released artifacts, including compatibility of controller
consumers if a callback API changed. Run a media-shaped transfer through the
real QUIC stack to confirm handshake sampling, pacing, and recovery work
together. This integration check is required; the broader media-flow study
and Google comparison do not gate these bug fixes.

## Related

- [Release the stack](/quest/m1/quic/release.md) - later feature releases follow the same packaging rules
- [BBR3 app-limited](/quest/m2/quic-bbr-app-limited.md) - broader media measurements after the corrected release
