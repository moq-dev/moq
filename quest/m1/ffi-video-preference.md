# [S] Bindings carry the video rendition preference

## Goal

moq-ffi's `MoqVideo` catalog record carries hang's `preference: i32`
(default 0), and every binding exposes it, so a binding consumer such as the
OBS source can rank renditions and sees a publisher's compatibility transcode
(`-1`). It ships in the breaking binding cut with the rest of m1's binding API
work.

## Plan

Add the field to `MoqVideo` in `rs/moq-ffi/src/media.rs` and follow the
Cross-Package Sync row for `rs/moq-ffi`: the generated and hand-written
wrappers, `cpp/moq`, and `doc/lib/*`.

Decided in review of
[moq-dev/moq#6366](https://github.com/moq-dev/moq/pull/6366) (maintainer):
split from the OBS rendition quest into m1 so it rides the breaking binding
cut instead of breaking bindings again later.

Public API: a new `preference` field on every binding's video catalog record.
It changes generated record checksums, positional constructors, and Go unkeyed
struct literals. So, like the earlier `epoch` addition, add a `## Unreleased`
entry to each affected package's `MIGRATE.md`. Wire: none.

## Related

- [OBS source renditions](/quest/m2/obs-source-renditions.md) - the consumer that ranks by it
- [A smaller, safer API than release](/quest/m1/api-audit/README.md) - the binding cut this rides
