# [M] TS import reports typed outcomes, and no caller downcasts an error

## Goal

`moq_mux::container::ts` reports what happened through types. Its public
functions return a `ts::Error` enum, not `anyhow`; a source switch comes
back as a value (`Restart`), not as an error that carries the unread
input; a stream with several programs is a variant a caller matches on.
`moq_mux::Error::Other` is gone. The callers, moq-cli (`import ts`,
`import srt`, and `import rtsp`) and moq-srt, read the typed result and
contain no `downcast_ref` or `is::<T>()` on an error.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Typed TS outcomes: a `ts::Error` enum, restart as a value,
  `Error::Other` dropped, and the CLI stops downcasting, rtsp included
  (rejected: keep `anyhow` inside and expose only the two outcomes as
  typed values; keep as is). AGENTS.md: "Never match on errors; it's
  brittle", and `rs/AGENTS.md` asks libraries for `thiserror`.

Facts found by the audit:

- `rs/moq-mux/src/container/ts/import.rs`: `pub fn decode(&mut self, data:
  &[u8]) -> anyhow::Result<()>`, with `seek` and `finish` the same. The
  internal `decode_or_restart` beside it already returns
  `anyhow::Result<Option<Restart>>`, the shape the public one should have.
  `programs.rs` (`decode`, `finish`) and `republish.rs` (`decode`,
  `finish`) return `anyhow` too.
- `MultipleProgramsError` (`import.rs`) is a second outcome sent as an
  error, and `moq_mux::Error::Other(Arc<anyhow::Error>)`
  (`rs/moq-mux/src/error.rs`) exists to carry it; 8 sites in
  `rs/moq-mux/src` build or name `Other`.
- Callers that match on the error:
  `rs/moq-cli/src/publish.rs` (`err.is::<ts::MultipleProgramsError>()`),
  `rs/moq-cli/src/srt.rs` (the same through two error layers),
  `rs/moq-srt/src/ts.rs` (`downcast_ref::<ts::MultipleProgramsError>()`),
  and `rs/moq-cli/src/rtsp.rs` (`downcast_ref::<moq_rtsp::Error>()`).
- [moq-dev/moq#8831](https://github.com/moq-dev/moq/pull/8831) moved the
  `Restart` handling into one `ts::Republish` but left the signatures.

For the PR:

- `ts::Error` is a `thiserror` enum, `#[non_exhaustive]`, with a variant
  for several programs that carries what the caller prints today, and
  variants for the failures the callers or tests tell apart. Do not
  enumerate every internal `anyhow!`: a damaged packet that the importer
  already counts and survives is not an error, and what remains groups
  into a handful of causes. It converts into `moq_mux::Error` by a typed
  variant, which is what lets `Other` go.
- `Import::decode` returns `Result<Option<Restart>, ts::Error>`;
  `Programs` and `Republish` return the same error. Internal helpers may
  keep `anyhow` only if the boundary maps every case; prefer converting
  them, since 18 public `ts` functions name `anyhow::Result` today.
- moq-rtsp: give the CLI what it downcasts for as a typed result from the
  call it makes, so `rs/moq-cli/src/rtsp.rs` matches a `moq_rtsp::Error`
  it was handed, not one it dug out of an `anyhow` chain.
- Out of scope: `rs/moq-cli/src/play/media.rs` downcasts the CLI's own
  private `Unplayable`, and `rs/moq-srt/src/dial.rs` reads a libsrt
  rejection from an `io::Error`; neither crosses a library API of ours.
- [moq-dev/moq#8830](https://github.com/moq-dev/moq/pull/8830) is open on
  the TS export; it does not touch these signatures.

Tests: the existing TS import tests assert on variants where they assert
on message text today; a multi-program input returns the variant from
`decode`; a source switch returns `Some(Restart)` and the importer keeps
decoding the next source.

Public API: breaking for Rust: the return types of `ts::Import`,
`ts::Programs`, and `ts::Republish`; `ts::MultipleProgramsError` becomes a
variant; `moq_mux::Error::Other` is removed. Wire: none. CLI: messages may
change wording, exit codes do not.

## Related

- [A multi-program TS import survives a source switch](/quest/m5/ts-multi-program-restart.md) - deferred; would add a restart value to `Programs`
- [Finishing an importer or the video producer returns a Finished handle](/quest/m2/finish-sweep.md) - the other change to importer signatures
