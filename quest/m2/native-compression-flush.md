# [S] Report native DEFLATE frame flush requirements

## Goal

The existing browser compression flush discussion has a reproducible MoQ
use case: each raw-DEFLATE frame becomes available without finishing the
group or losing its shared dictionary. Record the report and its upstream
link here; shipping a browser API is separate work.

## Plan

Decided with the maintainer on 2026-10-21: pursue native codecs as follow-up
work while the approved synchronous JS codec implementation proceeds.

- Contribute to [on-demand flush](https://github.com/whatwg/compression/issues/56),
  checking [compression options](https://github.com/whatwg/compression/issues/22)
  and [stream flushing](https://github.com/whatwg/streams/issues/960) first.
  Reuse the existing discussion instead of filing another feature request.
- Provide a standalone small-frame reproduction with a concurrent reader.
  MoQ uses raw DEFLATE per group, a nonfinal sync flush per frame, and shared
  history. Closing the compressor per frame changes those semantics.
- Initial Linux observation: Chromium 156.0.8078.4, Playwright build 1248,
  accepted two 224-byte writes but emitted no bytes after either resolved
  write and one event-loop turn; close emitted 39 bytes. Chromium's
  `deflate_transformer.cc` uses `Z_NO_FLUSH` for writes and `Z_FINISH` at
  close. This is a missing flush facility, not evidence of a spec violation.
- Recheck current available Chromium, Firefox and Safari builds. Record exact
  versions and unavailable engines; never generalize one engine's result to
  the others. File an engine defect only with a distinct reproduced failure.
- The same Chromium test decoded two shared-window MoQ slices of 34 and 10
  bytes into 224 bytes each before closure after restoring `00 00 ff ff`.
  No decoder buffering bug was reproduced. Closing the nonfinal stream
  errors as truncated; appending a local empty final block `03 00` allows
  clean closure. Do not report expected end-of-stream validation as a bug.
- `Z_SYNC_FLUSH` is a codec operation, not a synchronous JavaScript API.
  Keep that separate from the synchronous codec report.
- Prepare concrete upstream text and reproductions before requesting any
  external-post authorization required by repository instructions. Record
  the actual upstream outcome without waiting indefinitely for adoption.

No public API, wire or new documentation page in this reporting work.

## Related

- [Synchronous codecs](/quest/m2/native-compression-sync.md) - stateful synchronous calls are a separate API requirement
- [Native adoption](/quest/m2/native-compression-adopt.md) - replace bundled code only when the native contract fits
