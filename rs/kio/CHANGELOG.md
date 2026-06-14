# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Reworked `Producer::poll` / `Producer::wait`. They previously handed the
  closure a `Mut` and auto-notified consumers whenever it touched the value via
  `DerefMut`. Since a no-op like `Vec::pop` on an empty queue still trips
  `DerefMut`, a pending poll would wake the polling task's own waiter and spin
  into an infinite loop. They now take a read-only predicate over a `Ref` and,
  on `Poll::Ready`, hand back a `Mut` with the lock still held so the caller
  mutates atomically without the footgun. `Weak::poll_write` / `Weak::wait` had
  no users and are removed.

## [0.3.0] - 2026-05-29

### Other

- Renamed from `conducer` to `kio`. The API is unchanged; only the crate name differs.
