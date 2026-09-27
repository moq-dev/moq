# [XS] NVENC reports its own keyframes

## Goal

The `moq-video` NVENC backend sets `Encoded::keyframe` from the picture type
NVENC reports for the output. It stops re-deriving the flag by scanning the
Annex-B slice type.

## Plan

- moq-nvenc already reads `pictureType` when it locks the output bitstream,
  but `Submission::finish` returns only the bytes. Expose the picture type, or
  a keyframe bool, on that result. It is a published crate, so weigh the
  shape against the other fields a caller may want later, such as the output
  timestamp.
- NVENC's IDR and I picture types are not the same thing. The flag should
  mean what the moq-mux importer treats as a group start (an H.264 IDR, an
  H.265 IRAP), so check both codecs on hardware.
- The NVIDIA tests skip under the Nix shell unless the driver libraries are on
  the loader path; see [NVDEC teardown](/quest/m1/nvdec-teardown.md).

## Related

- [#4295](https://github.com/moq-dev/moq/pull/4295) - added the flag, with NVENC on the bitstream fallback
