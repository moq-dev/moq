# [M] C decoder output configuration before release

## Goal

C callers select the decoded CPU pixel format and target size through
`moq_video_decoder_output` before its layout is released. Accepted requests
produce that output, or fail explicitly. GPU frame access remains separate.

## Plan

The existing `#[repr(C)]` output struct contains only `max_age_ms`. Adding
format or size fields changes its layout and ABI; it is not an additive change.
Make this change on `dev`, update all callers and regenerate `moq.h` together.
Remove the claim that reserved configuration structs make future fields additive.

Use existing `moq_video::Frame` CPU conversion and resize operations to honor
supported requests now. Decoder resize and GPU preferences are backend hints,
not proof of the requested dimensions or pixel layout: validate the result and
convert when needed. Preserve the current I420/native-size default. Define
validation for dimensions and supported formats explicitly; do not accept an
option that is ignored until M2. Keep format and size types consistent with
existing native definitions where the C ABI permits it.

Use a compiled C consumer fixture in CI to verify the new layout, default
behavior, requested CPU format/size, and refusal of unsupported requests.
Update `doc/lib/c/index.md` and affected OBS consumers. Record the ABI break
in the PR and release proof. Do not add a compatibility entry point or a second
frame representation.

## Related

- [Decoded frames](/quest/m2/decoded-frames.md) - shared backing and consumer-specific views
- [C group fetch](/quest/m2/libmoq-fetch.md) - independent additive entry point
- [External API proof](/quest/m1/api-release-proof.md) - packaged C caller validation
