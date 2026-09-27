# [S] Dropping an NVDEC decoder does not crash

## Goal

Dropping a `moq-video` NVDEC decoder releases it without a segfault, and the
hardware tests `nvdec_h264_round_trip` and `nvdec_resize_scales_output` pass.

## Plan

Both tests die with SIGSEGV on `main` on an RTX 3070 Ti (driver 595.91), while
`nvdec_h265_round_trip` and `nvdec_to_nvenc_zero_copy` pass. The backtrace
ends in `cuEventDestroy_v2`, called by `cuvidDestroyDecoder` from
`<nvdec::Decoder as Drop>::drop` while the test drops the backend. That
`Drop` assumes the caller keeps the CUDA context bound, which nothing
enforces. Suspect the context is not current on the dropping thread, or is
released before the decoder; confirm before fixing.

Under the Nix dev shell the driver libraries are not on the loader path, so
the NVIDIA tests skip. To run them, expose `libcuda`, `libnvidia-encode`, and
`libnvcuvid` from `/usr/lib/x86_64-linux-gnu` through a directory of symlinks
on `LD_LIBRARY_PATH`.
