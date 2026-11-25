# [XS] Media Foundation opens an H.265 decoder

## Goal

On Windows with the HEVC Video Extensions, moq-video's Media Foundation
backend opens and decodes an H.265 track instead of failing with
`MF_E_TRANSFORM_TYPE_NOT_SET`, and H.264 still decodes.

## Plan

Reported in [moq-dev/moq#9448](https://github.com/moq-dev/moq/issues/9448).
Verified at `888de61a12`: `open` sets an input type with only the major type
and subtype, then sends `BEGIN_STREAMING`
(`rs/moq-video/src/decode/backend/mediafoundation.rs:156-187`), and sets the
output type only after the first `ProcessInput`. Media Foundation wants every
type set first. The H.264 decoder tolerates the early message, and the HEVC
decoder refuses it. This is as old as the backend, with no single culprit PR.

Decided 2026-11-23 (maintainer, /quest-import):

- ✅ Land it by finishing tosscaster's
  [moq-dev/moq#9449](https://github.com/moq-dev/moq/pull/9449) (rejected:
  reviewing it with no quest). It allows maintainer edits. It sets a placeholder
  `MF_MT_FRAME_SIZE` on the input type and the output type before
  `BEGIN_STREAMING`, and the existing `MF_E_TRANSFORM_STREAM_CHANGE` handling
  in `process_output` picks up the real size.
- Finish it by deleting the now-unreachable `output_configured` field and its
  branches. Prefer `Stream.size` (`decode/backend/mod.rs:103`) over the
  placeholder when the description pins it.
- ✅ tosscaster re-tests the final head (rejected: the maintainer's windows
  host). CI can't: the Windows runner has no GPU or HEVC extension.

Public API: none. Wire: none.

## Waiting

- tosscaster: re-run their H.264 and H.265 decode checks (B-frames, aperture crop, flush, luma against ffmpeg) on #9449's final head; check for their result on the PR

## Closes

- [#9448](https://github.com/moq-dev/moq/issues/9448) - Media Foundation H.265 decode fails to open
