# OBS native codecs

## Goal

Remove the MoQ OBS plugin's dependency on OBS/system FFmpeg ABI versions by decoding subscribed video with moq-video. Add audio playback and publishing with moq-audio, and opt-in video publishing with moq-video. OBS retains scene composition, audio mixing, and output timing. This integration serves MoQ publishing and playback, not general OBS recording or other streaming outputs.

## Plan

Portability and FFmpeg removal lead. The current MoQ source uses libavcodec, libavutil, and libswscale for video; it has no audio playback. Its swresample linkage is unused. Video replacement can therefore remove direct FFmpeg dependencies without waiting for audio. The plugin reaches codecs through the generated C++ package over moq-ffi (the migration quest linked below lands first); build `libmoq_ffi` statically with only the codec features needed here; OS frameworks and runtime GPU drivers remain valid dependencies. Verify plugin imports instead of promising a completely static OBS plugin.

Attempt GPU delivery immediately, starting on macOS. Windows and Linux can ship independently. Prefer direct surface reuse, allow GPU conversion/blits, and automatically fall back to CPU delivery when import is unavailable or fails. Stats must show the actual decoder/encoder, delivery path, and fallback reason. Retaining a texture handle is insufficient unless pool ownership and synchronization also prevent reuse while work is in flight.

Initial video decoding covers H.264, HEVC, and AV1 where moq-video has an available backend. Unsupported codecs produce an actionable error; do not retain an FFmpeg fallback. VP8/VP9 return through their own follow-up quest. Audio playback covers Opus, AAC-LC, and PCM.

Publishing remains opt-in, with one **Use MoQ encoders** choice for video and audio. Keep the existing OBS encoder mode. Internal OBS encoder adapters call moq-video/moq-audio, preserving OBS's A/V handling and the existing encoded MoQ output. The combined choice is enabled only when both adapters are present. Start with H.264, supported HEVC, and Opus; defer AV1/AAC encoding and PCM publishing UI. Keep bitrate separate from **Low latency** (default), **Balanced**, and **Quality** presets. Presets describe supported buffering/compression controls, not an end-to-end delay promise.

The quests separate portable decoding, platform GPU delivery, audio, and publishing so each can land and be validated independently. moq-ffi's decoded frames own their surface and convert to CPU pixels only on request; a native decode exposes the platform surface as a borrowed view, which each platform quest extends to its own surface type.

## Quests

- [Video source replacement](/quest/m1/obs-moq-video/source.md) - remove FFmpeg and attempt macOS GPU delivery immediately, with a working CPU fallback on other platforms
- [Audio playback](/quest/m1/obs-moq-video/audio-playback.md) - add synchronized subscribed audio through moq-audio
- [Windows decoded frames](/quest/m1/obs-moq-video/decode-windows.md) - present decoded D3D11 surfaces in OBS without CPU readback
- [Linux decoded frames](/quest/m1/obs-moq-video/decode-linux.md) - present supported native decoded surfaces with visible CPU fallback
- [Linux bundle](/quest/m1/obs-moq-video/linux-bundle.md) - attach a portable Linux x86_64 tarball to every obs-moq release once FFmpeg is gone
- [Encoder presets](/quest/m1/obs-moq-video/presets.md) - define and measure shared low-latency, balanced, and quality policies
- [Audio publishing](/quest/m1/obs-moq-video/audio-publish.md) - back an internal OBS Opus encoder with moq-audio
- [Video publishing](/quest/m1/obs-moq-video/adapter.md) - back an internal OBS video encoder with moq-video and expose the combined opt-in mode
- [Rate control](/quest/m1/obs-moq-video/rate-control.md) - the plugin reserves its bitrate and retunes the OBS encoder to the grant
- [macOS GPU input](/quest/m1/obs-moq-video/macos.md) - feed the encoder from the OBS compositor without CPU readback
- [Windows GPU input](/quest/m1/obs-moq-video/windows.md) - import or blit OBS D3D11 textures with explicit synchronization
- [VP8/VP9 decoding](/quest/m1/obs-moq-video/vpx.md) - restore those playback codecs without an FFmpeg ABI dependency

## Related

- [OBS migration](/quest/m1/cpp/obs.md) - every quest here starts from the plugin on the generated C++, so codec surface is designed in moq-ffi and reaches libmoq and the other wrappers through the Cross-Package Sync table, not as OBS-only C symbols
- [Linux GPU input](/quest/m2/obs-linux-gpu.md) - allocation-export feasibility and its dependent implementation are deferred
- [VAAPI encode and decode](/quest/m4/video-vaapi.md) - owns Linux backend decode/import capabilities; reconcile its older dependency assumptions against current code
- [Video hardware validation](/quest/m3/video-hardware.md) - physical hardware evidence is required for each claimed GPU path
- [Audio codecs](/quest/m1/audio-codecs/README.md) - HE-AAC, multichannel, and native AAC encode reach the OBS source and encoder adapters through moq-ffi
