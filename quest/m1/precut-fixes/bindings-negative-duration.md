# [S] Swift and Go refuse a negative duration with an error, not a crash

## Goal

Every duration a Swift or Go caller passes to the bindings is checked by the
wrapper, so a negative, infinite, or NaN value returns the wrapper's config
error instead of aborting the app, as Kotlin and Python already throw a
catchable exception.

## Plan

Found by the 2026-11-17 pre-cut correctness audit, from source. The
generated converters call `fatalError` (Swift, from the pinned UniFFI fork)
or `panic` (Go) on a negative duration. Go guards every duration except
`EncoderOutput.Join` (`go/wrapper/video/video.go`). Swift guards none:
`Client(timeout:race:resolutionDelay:)` is documented to throw a config error
and aborts instead, and the JSON and media `delay`, `TrackInfo.maxAge`,
`Backoff`, and `VideoEncoderOutput.join` do the same. Neither API is on
`release`.

Recommended: check in each wrapper before building the record, with a test
per wrapper. Fixing the generator fork instead would also fix any future
duration; prefer it if the fork takes the change cheaply
([UniFFI forks](/quest/m3/vendor-sync/uniffi.md)).

Public API: none. Wire: none.
