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
the batch read path; profile it first, then report progress per batch or
only when the pacing window can change.

Counted 2026-11-05 in [moq-dev/moq#7874](https://github.com/moq-dev/moq/pull/7874)
with `just bench-fanout <revisions> group`, which drains a 512-frame group as
`group_relay_frames/plain/512` does: pacing adds 3,007 instructions per drained
group (149,368 to 152,375, +2.0%). Most of the loaded host's 15% from
`e7a5c10` to `f37d964` was [moq-dev/moq#7504](https://github.com/moq-dev/moq/pull/7504)'s
idle gate on every frame read (+12.7%), which that PR took back with more: the
drain is now 135,396, 9.4% below pacing's parent, pacing included (see the
[report](/.github/relay-cpu-bisect.md#group_relay_frames-since-e7a5c10)).
