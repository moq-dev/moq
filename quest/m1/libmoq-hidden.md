# [XS] libmoq hidden opt-in

## Goal

C callers can list hidden broadcasts (a `.`-prefixed segment below the
prefix) the way every other binding can: `moq_origin_announced` takes a
`hidden` flag, mirroring `MoqAnnounceConfig.hidden` in moq-ffi. Today a C
caller can only list them by naming the dot segment in `prefix`.

## Plan

- Adding a parameter breaks the C ABI, so this lands on `dev`.
- Update `rs/libmoq/src/{api,origin}.rs`, the libmoq tests, `cpp/obs` if it
  calls `moq_origin_announced`, and `doc/lib/c/index.md`.
