# [XS] Restart the nightly size history after the libmoq rename

## Goal

Nightly's size report passes again once
[moq-dev/moq#8941](https://github.com/moq-dev/moq/pull/8941) merges. That PR
renames the hand-written C crate's four release archives from
`moq-c-<target>` to `libmoq-<target>` and stops matching the generated
package's `moq-c-generated-<target>` uploads, so a retained history that names
the old keys makes `sh/size/report.py` refuse every report as "missing or
renamed" until the history restarts.

## Plan

After that PR merges, delete every retained `nightly-size-history` artifact
with the command in `sh/size/README.md`, as an agent with repository admin
access can, then confirm the next scheduled Nightly's `Collect and compare
sizes` step passes. The restart drops the 30-day baseline of every series, so
the first 30 days compare only against the previous night.

## Waiting

- moq-dev/moq: [moq-dev/moq#8941](https://github.com/moq-dev/moq/pull/8941) merges; check with `gh pr view 8941 --json state`
