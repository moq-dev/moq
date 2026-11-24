# OBS native codecs

## Goal

Remove the MoQ OBS plugin's dependency on OBS/system FFmpeg ABI versions by decoding subscribed video with moq-video and subscribed audio with moq-audio. Add audio publishing with moq-audio, and opt-in video publishing with moq-video. OBS retains scene composition, audio mixing, and output timing. This integration serves MoQ publishing and playback, not general OBS recording or other streaming outputs.

## Plan

Portability and FFmpeg removal lead. The current MoQ source decodes video with libavcodec, libavutil, and libswscale; audio already decodes through moq-audio. Its swresample linkage is unused. The video source replacement removes the remaining FFmpeg linkage. The plugin reaches codecs through the generated C++ package over moq-ffi, so codec surface is designed in moq-ffi and reaches the other bindings through the Cross-Package Sync table, not as OBS-only C symbols; build `libmoq_ffi` statically with only the codec features needed here; OS frameworks and runtime GPU drivers remain valid dependencies. Verify plugin imports instead of promising a completely static OBS plugin.

Attempt GPU delivery immediately on macOS decode; other platforms keep the CPU path here. Prefer direct surface reuse, allow GPU conversion/blits, and automatically fall back to CPU delivery when import is unavailable or fails. Stats must show the actual decoder/encoder, delivery path, and fallback reason. Retaining a texture handle is insufficient unless pool ownership and synchronization also prevent reuse while work is in flight.

Initial video decoding covers H.264, HEVC, and AV1 where moq-video has an available backend. Unsupported codecs produce an actionable error; do not retain an FFmpeg fallback. VP8/VP9 return through their own follow-up quest. Audio playback covers Opus, AAC-LC, and PCM.

Publishing remains opt-in, with one **Use MoQ encoders** choice for video and audio. Keep the existing OBS encoder mode. Internal OBS encoder adapters call moq-video/moq-audio, preserving OBS's A/V handling and the existing encoded MoQ output. The combined choice is enabled only when both adapters are present. Start with H.264, supported HEVC, and Opus; defer AV1/AAC encoding and PCM publishing UI. Keep bitrate separate from **Low latency**, **Balanced** (default), and **Quality** presets. Presets describe supported buffering/compression controls, not an end-to-end delay promise. The line PR must call out the video default changes #4099 made: NVENC P4 to P1, and openh264 medium to low complexity. Known gap: VAAPI reports Low latency whatever was asked, and V4L2 and MediaCodec report unconfirmed; no quest owns measuring and mapping presets for them. Media Foundation has no owner either.

The quests separate portable decoding, audio, and publishing so each can land and be validated independently. moq-ffi's decoded frames own their surface and convert to CPU pixels only on request (#4094); a surface decode exposes the platform surface as a borrowed view, which each platform GPU quest extends to its own surface type.

## Related

- [Video hardware validation](/quest/m5/video-hardware.md) - physical hardware evidence is required for each claimed GPU path
- [AudioToolbox decode](/quest/m3/audio-decode-audiotoolbox.md) - HE-AAC and multichannel AAC reach the OBS source on macOS through moq-ffi
- [AudioToolbox encode](/quest/m3/audio-encode-audiotoolbox.md) - native AAC encode reaches the OBS encoder adapter on macOS through moq-ffi
