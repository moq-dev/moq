# [S] TLS reload follows symlinked certificate files

## Goal

Rewriting a certificate or key in place reloads it when the configured path
is a symlink whose target lives outside the configured file's directory. This
covers roots, served chains, and client certificates.

## Plan

Decided 2026-11-05 (maintainer, follow-up from
[moq-dev/moq#7906](https://github.com/moq-dev/moq/pull/7906)): this joined what was then m2, later work, which is m3 since the 2026-11-05 re-cut.

`watch::callback` (`rs/moq-tokio/src/watch.rs`) watches each configured
file's directory. Its filter already reloads when a path resolves somewhere
new, which covers a Kubernetes `..data` swap and a repointed link, but an
in-place write to a target in an unwatched directory raises no event. Also
watch the directories the configured paths resolve to, and move those watches
when a resolution changes. Tests drive the watcher with controlled events, not
wall-clock waits.
