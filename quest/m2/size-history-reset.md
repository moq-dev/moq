# [XS] Restart the nightly size history after the minimal FFI series ends

## Goal

Nightly's size report passes again after
[moq-dev/moq#8851](https://github.com/moq-dev/moq/pull/8851) removes the
`libmoq_ffi-minimal` series: a retained history that names it makes
`sh/size/report.py` refuse every report as "missing or renamed", so the job
stays red until the history restarts.

## Plan

After that PR merges, delete every retained `nightly-size-history` artifact with
the command in `sh/size/README.md`, as an agent with repository admin access can,
then confirm the next scheduled Nightly size step passes. The restart drops the
30-day baseline of every series, so the first 30 days compare only against the
previous night.

## Waiting

- moq-dev/moq: [moq-dev/moq#8851](https://github.com/moq-dev/moq/pull/8851) merges; check with `gh pr view 8851 --json state`
