# [S] TLS reload follows symlinked certificate files

## Goal

Rewriting a certificate or key in place reloads it even when the configured
path is a symlink whose target lives in another directory, as Kubernetes
secret mounts do. This covers roots, served chains, and client certificates.

## Plan

Decided 2026-11-05 (maintainer, follow-up from
[moq-dev/moq#7906](https://github.com/moq-dev/moq/pull/7906)): ✅ m2.

`watch::callback` (`rs/moq-tokio/src/watch.rs`) watches each configured
file's directory, so a rewrite of a symlink target elsewhere goes unseen.
Also watch the directories the configured paths resolve to, and re-resolve
them when a link changes, so a swapped `..data` link is followed. Tests drive
the watcher with controlled events, not wall-clock waits.
