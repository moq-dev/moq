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

Verify the fork's regression suite and MoQ's default and supported runtime
builds against the released artifacts, including compatibility of controller
consumers if a callback API changed. Run a media-shaped transfer through the
real QUIC stack to confirm handshake sampling, pacing, and recovery work
together. This integration check is required; the broader media-flow study
and Google comparison do not gate these bug fixes.

## Required

- [Preserve QUIC packet identity in BBR](/quest/m1/quic/bbr-packet-identity.md)
- [Finish each BBR ACK sample before using it](/quest/m1/quic/bbr-ack-sampling.md)
- [Mark application starvation before the next BBR send](/quest/m1/quic/bbr-app-limited.md)
- [Finish BBR bandwidth-probe feedback once](/quest/m1/quic/bbr-probe-feedback.md)
- [Recalibrate BBR startup pacing from measured RTT](/quest/m1/quic/bbr-startup-pacing.md)
- [Protect bandwidth samples during BBR ProbeRTT](/quest/m1/quic/bbr-probe-rtt.md)
- [Preserve BBR state across a spurious loss episode](/quest/m1/quic/bbr-loss-undo.md)

## Related

- [Release the stack](/quest/m1/quic/release.md) - later feature releases follow the same packaging rules
- [BBR3 app-limited](/quest/m2/quic-bbr-app-limited.md) - broader media measurements after the corrected release
