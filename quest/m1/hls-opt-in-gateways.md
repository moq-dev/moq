# [S] HLS records opt-in for gateways and bindings

## Goal

Every publisher that builds its own catalog can opt in to HLS records, so
`export hls` can serve its broadcasts: the SRT, RTMP, and WebRTC ingest
gateways (`moq-srt`, `moq-rtmp`, `moq-rtc`) and the native bindings
(`moq-ffi` and its wrappers, `moq-c`, `moq-gst`). Today only the CLI's own
imports take `--hls`, and `import srt`, `import rtmp`, and `import rtc` refuse
it.

## Plan

- Each gateway's publish builder gains the opt-in beside `with_max_age`, and
  `moq import --hls` passes it through instead of refusing.
- The bindings expose the opt-in where they build a catalog config, mirrored
  across languages.
- Update `doc/bin/cli.md`, `doc/bin/hls.md`, and the binding docs.

Public API: one opt-in per gateway builder and binding. Wire: none.

## Related

- [JS HLS field](/quest/m2/js-hls.md) - the `@moq/publish` opt-in
