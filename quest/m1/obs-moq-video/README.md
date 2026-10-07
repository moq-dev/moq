# OBS native codecs

## Goal

Remove the MoQ OBS plugin's dependency on OBS/system FFmpeg ABI versions by decoding subscribed video with moq-video and subscribed audio with moq-audio. Add audio publishing with moq-audio, and opt-in video publishing with moq-video. OBS retains scene composition, audio mixing, and output timing. This integration serves MoQ publishing and playback, not general OBS recording or other streaming outputs.

## Plan

Portability and FFmpeg removal lead. The current MoQ source decodes video with libavcodec, libavutil, and libswscale, and audio with libavcodec (`moq_source_decode_audio_frame` in `cpp/obs/src/moq-source.cpp`). Its swresample linkage is unused. FFmpeg linkage goes away only once both the video source replacement and the audio decode replacement land. The stranded audio branch (#3498, merged only into `codex/obs-audio-receive-base`) is abandoned; the audio playback quest replans it on the generated C++. The plugin reaches codecs through the generated C++ package over moq-ffi (the migration quest linked below lands first); build `libmoq_ffi` statically with only the codec features needed here; OS frameworks and runtime GPU drivers remain valid dependencies. Verify plugin imports instead of promising a completely static OBS plugin.

Attempt GPU delivery immediately on macOS decode; other platforms keep the CPU path here. Prefer direct surface reuse, allow GPU conversion/blits, and automatically fall back to CPU delivery when import is unavailable or fails. Stats must show the actual decoder/encoder, delivery path, and fallback reason. Retaining a texture handle is insufficient unless pool ownership and synchronization also prevent reuse while work is in flight.

Initial video decoding covers H.264, HEVC, and AV1 where moq-video has an available backend. Unsupported codecs produce an actionable error; do not retain an FFmpeg fallback. VP8/VP9 return through their own follow-up quest. Audio playback covers Opus, AAC-LC, and PCM.

Publishing remains opt-in, with one **Use MoQ encoders** choice for video and audio. Keep the existing OBS encoder mode. Internal OBS encoder adapters call moq-video/moq-audio, preserving OBS's A/V handling and the existing encoded MoQ output. The combined choice is enabled only when both adapters are present. Start with H.264, supported HEVC, and Opus; defer AV1/AAC encoding and PCM publishing UI. Keep bitrate separate from **Low latency**, **Balanced** (default), and **Quality** presets. Presets describe supported buffering/compression controls, not an end-to-end delay promise. The line PR must call out the video default changes #4099 made: NVENC P4 to P1, and openh264 medium to low complexity. Known gap: VAAPI reports Low latency whatever was asked, and V4L2 and MediaCodec report unconfirmed; no quest owns measuring and mapping presets for them.

The quests separate portable decoding, audio, and publishing so each can land and be validated independently. moq-ffi's decoded frames own their surface and convert to CPU pixels only on request (#4094); a surface decode exposes the platform surface as a borrowed view, which each platform GPU quest extends to its own surface type.

Decided in the 2026-09-30 audit: the Windows and Linux GPU decode paths and the macOS and Windows GPU encoder inputs moved to m2 (listed under Related), because each needs physical-hardware proof, and Linux GPU input is already m2/m3.

## Required

- [Video source replacement](/quest/m1/obs-moq-video/source.md) - remove the FFmpeg video decode and attempt macOS GPU delivery immediately, with a working CPU fallback on other platforms
- [Audio playback](/quest/m1/obs-moq-video/audio-playback.md) - replace the FFmpeg audio decode with moq-audio
- [Linux bundle](/quest/m1/obs-moq-video/linux-bundle.md) - attach a portable Linux x86_64 tarball to every obs-moq release once FFmpeg is gone
- [Audio publishing](/quest/m1/obs-moq-video/audio-publish.md) - back an internal OBS Opus encoder with moq-audio
- [Video publishing](/quest/m1/obs-moq-video/adapter.md) - back an internal OBS video encoder with moq-video and expose the combined opt-in mode
- [Rate control](/quest/m1/obs-moq-video/rate-control.md) - the plugin reserves its bitrate and retunes the OBS encoder to the grant
- [VP8/VP9 in OBS](/quest/m1/obs-moq-video/vpx-obs.md) - play VP8 and VP9 through moq-video's libvpx backend on every OBS platform

## Related

- [C++ through moq-ffi](/quest/m1/cpp/README.md) - every quest here starts from the plugin on the generated C++, so codec surface is designed in moq-ffi and reaches the other bindings through the Cross-Package Sync table, not as OBS-only C symbols
- [Linux GPU decode](/quest/m2/obs-decode-linux.md) - present hardware-decoded frames without CPU readback; needs physical-hardware proof
- [Windows GPU decode](/quest/m2/obs-decode-windows.md) - present D3D11 decoded textures without CPU readback; needs physical-hardware proof
- [macOS GPU input](/quest/m2/obs-macos.md) - feed compositor frames to VideoToolbox without a CPU round trip; needs physical-hardware proof
- [Windows GPU input](/quest/m2/obs-windows.md) - feed D3D11 compositor frames to the encoder without CPU staging; needs physical-hardware proof
- [Video hardware validation](/quest/m3/video-hardware.md) - physical hardware evidence is required for each claimed GPU path
- [AudioToolbox decode](/quest/m1/audio-decode-audiotoolbox.md) - HE-AAC and multichannel AAC reach the OBS source on macOS through moq-ffi
- [AudioToolbox encode](/quest/m1/audio-encode-audiotoolbox.md) - native AAC encode reaches the OBS encoder adapter on macOS through moq-ffi
- [Linux GPU input](/quest/m3/obs-linux-gpu.md) - allocation-export feasibility and its dependent implementation are deferred
- [VAAPI encode and decode](/quest/m2/video-vaapi.md) - owns Linux backend decode/import capabilities; reconcile its older dependency assumptions against current code
