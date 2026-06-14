# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Replaced the `Mut`-closure poll APIs (`Producer::poll`, `Producer::wait`,
  `Weak::poll_write`, `Weak::wait`) with `Producer::poll_drain` /
  `Weak::poll_drain`. The old variants auto-notified consumers whenever the
  closure touched the value via `DerefMut`, which could wake the polling task's
  own waiter and spin into an infinite loop. The replacements hand out plain
  `&mut T` and never notify. Use `write()` to mutate-and-notify.

## [0.3.0] - 2026-05-29

### Other

- Renamed from `conducer` to `kio`. The API is unchanged; only the crate name differs.
