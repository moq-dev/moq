# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.1](https://github.com/moq-dev/moq/compare/kio-v0.4.0...kio-v0.4.1) - 2026-06-30

### Added

- *(moq-net)* add OriginProducer::dynamic + infallible OriginConsumer::request_broadcast ([#1913](https://github.com/moq-dev/moq/pull/1913))

## [0.4.0](https://github.com/moq-dev/moq/compare/kio-v0.3.0...kio-v0.4.0) - 2026-06-16

### Fixed

- *(kio)* split waiters by condition so writes don't churn closed/consumer waiters ([#1739](https://github.com/moq-dev/moq/pull/1739))
- *(moq-net)* release cached state when a producer is aborted or dropped ([#1715](https://github.com/moq-dev/moq/pull/1715))

### Other

- rework Producer::poll/wait to a read-only predicate that returns a Mut ([#1735](https://github.com/moq-dev/moq/pull/1735))

### Fixed

- Split the internal waiter list into separate value / closed / consumer lists,
  so an event only wakes the waiters that care about it. Previously every value
  modification (the hot path) also woke parked `closed()` and `used`/`unused`
  waiters, which re-registered and ping-ponged. No public API change.

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
