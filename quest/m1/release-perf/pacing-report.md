# [S] Pacing's per-frame report costs relay fan-out nothing measurable

## Goal

Relay fan-out reads (`group_relay_frames`) with fastest-reader pacing are
within noise of the pre-pacing parent for groups of 512 or more frames,
where today they are 4.6-8.1% slower, with pacing still bounding relay
memory exactly as before.

## Plan

Planned 2026-11-04 (maintainer): pacing's cost was accepted to finish
[fastest-reader pacing](https://github.com/moq-dev/moq/pull/7664), with this
trim as its follow-up. Measured in that PR, six rounds on a loaded host:
[moq-dev/moq#7388](https://github.com/moq-dev/moq/pull/7388), which added
pacing, against its parent `b51eb8c` is 4.6-8.1% slower on
`group_relay_frames` in all 12 cases with 512+ frames (64-byte payloads, the
worst case for a per-frame cost); 4-frame groups and `group_read_frames` are
unchanged. The likely cause is the per-read progress report #7388 added to
the batch read path; profile it first (`just bench-profile instructions`, as
[the relay CPU bisect](/quest/m1/release-perf/relay-cpu-bisect.md) does),
then report progress per batch or only when the pacing window can change.
