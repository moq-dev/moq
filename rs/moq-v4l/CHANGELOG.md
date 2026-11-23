# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Breaking:** `v4l2::open` returns an `OwnedFd` that closes the device when dropped, and `v4l2::close` is removed: it closed any fd from safe code.
- **Breaking:** `io::mmap::Stream` has no lifetime parameter; write `Stream` for `Stream<'static>`.

### Fixed

- Dropping a stream, its buffers, or a device handle never panics, whatever error tearing down returns. A userptr stream the driver cannot release leaks its buffers rather than freeing memory the driver may still write.
- `OutputStream::next` on the mmap stream fills every buffer once before it waits, waits on the dequeue instead of before the first queue, and can be called again after it fails without queueing a buffer twice. It used to time out on every frame.
- `set_timeout` saturates at `i32::MAX` milliseconds instead of panicking, `Handle::poll` restarts after `EINTR`, `v4l2::open` returns `InvalidInput` for a path with a NUL byte instead of panicking, and `with_buffers` fails when the driver grants no buffers.

- `CaptureStream::next` on the mmap and userptr streams can be called again after it returns `ErrorKind::TimedOut`, including on the first read after a restart. It used to queue a buffer the driver already held, which failed with EINVAL.

## [0.0.1](https://github.com/moq-dev/moq/releases/tag/moq-v4l-v0.0.1) - 2026-09-23

### Added

- *(video)* fork v4l in-tree with checked-in V4L2 bindings ([#3867](https://github.com/moq-dev/moq/pull/3867))
