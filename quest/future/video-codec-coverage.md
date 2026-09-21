# [M] Prioritize remaining native and portable video codec coverage

## Goal

Identify which missing decoder/backend coverage serves actual consumers after
the existing platform and NVIDIA work, without adding mandatory build costs or
blocking 0.1.

## Plan

Review AV1 decode through Apple VideoToolbox and Windows Media Foundation,
and a portable software AV1 fallback such as
[dav1d](https://code.videolan.org/videolan/dav1d). Establish actual framework,
OS, extension, hardware, format, and fixture requirements; a browser playing
a codec is not proof that our native backend can open it.

NVIDIA AV1 encode/10-bit, VAAPI expansion, and VP8/VP9 already have quests.
Keep those owners. Windows NVIDIA support needs a demonstrated advantage over
the existing native path. If revisiting software AV1 encoding, measure the
target real-time workload and build cost rather than assuming all presets or
all hardware are equivalent. Optional codec dependencies stay optional.

Produce a supported/refused matrix and separately completable implementation
quests only for justified gaps. Each selected backend needs decode fixtures,
drain/color/timestamp proof, and a CI or explicit hardware validation lane.
Use the settled Frame/output/codec extension points; no parallel surface API.

Public API and wire: no changes during this study.

## Related

- [NVIDIA formats](/quest/future/2147-moq-video-10-bit-hevc-and-av1-support-in-the-nvidia-codec.md) - existing AV1 encode and 10-bit scope
- [VAAPI](/quest/future/video-vaapi.md) - existing Linux codec expansion
- [VP8/VP9](/quest/next/obs-moq-video/vpx.md) - existing portable decoder scope
