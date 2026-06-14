# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Replaced the `Mut`-closure poll APIs (`Producer::poll`, `Producer::wait`,
  `Weak::poll_write`, `Weak::wait`) with `Producer::poll_write_when`. The old
  variants handed the closure a `Mut` and auto-notified consumers whenever it
  touched the value via `DerefMut`. Since a no-op like `Vec::pop` on an empty
  queue still trips `DerefMut`, a pending poll would wake the polling task's own
  waiter and spin into an infinite loop. `poll_write_when` evaluates a read-only
  predicate over a `Ref`, then on `Poll::Ready` hands back a `Mut` (lock still
  held) so the caller mutates atomically without the footgun.

## [0.3.0] - 2026-05-29

### Other

- Renamed from `conducer` to `kio`. The API is unchanged; only the crate name differs.
