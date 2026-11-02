# [XS] One nextest pin

## Goal

The cargo-nextest version is pinned in one place. Today it's written three
times, all at 0.9.146: `platform.yml`'s Windows and macOS jobs
(`tool: nextest@0.9.146`), and `.oss-scanner/Dockerfile`'s `NEXTEST_VERSION`
build arg.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-complete run). Raised by [moq-dev/moq#7217](https://github.com/moq-dev/moq/pull/7217),
which pinned the Dockerfile to match. Use whatever mechanism the workflows
already share for pins, such as a shared action, a repo variable, or a file
the jobs read, and keep the Dockerfile reading the same value.
