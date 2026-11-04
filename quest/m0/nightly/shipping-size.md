# [S] Nightly's shipping-size check passes

## Goal

Nightly's shipping-size check passes on `main`. Every growth over its 5%
threshold is traced to the commits that caused it. Each is then shrunk back,
or accepted as a new baseline with the reason recorded.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7667](https://github.com/moq-dev/moq/pull/7667)): milestone m0,
a child of the Nightly epic.

The scheduled run of 2026-11-03
([job](https://github.com/moq-dev/moq/actions/runs/41656613559/job/126039954216))
failed on growth since the previous run:

- `js/net` raw went from 444,692 to 467,348 bytes.
- `js/watch` also grew past the threshold.
- All four moq-relay archives grew to about 10.2–11.3 MB, from about
  9.5–10.3 MB.

Bisect each artifact between the two runs' commits. Likely suspects are the
recent bigint group IDs ([moq-dev/moq#7681](https://github.com/moq-dev/moq/pull/7681))
for js/net, and any new default feature or dependency for moq-relay.
`cargo bloat` and the bundle's metafile name the code that grew.

If the threshold compares only against the previous run, one intended jump
fails every night until the baseline moves. Check how the job sets its
baseline, and give it a documented way to accept a reviewed growth.
[Size audit](/quest/m2/audit-size.md) owns the release-time view of these
sizes.

Public API: none. Wire: none.

## Related

- [Size audit](/quest/m2/audit-size.md) - pre-cut size investigation and the post-release refresh
