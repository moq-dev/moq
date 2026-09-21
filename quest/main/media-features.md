# [M] Optional native compilation and opt-in rendering

## Goal

A consumer can build these media crates without OpenH264's C++ compilation or
the rendering stack. Defaults retain a working software H.264 fallback and
the existing inexpensive native backends.

## Plan

Make OpenH264 and its sys dependency one optional backend feature, enabled by
default. Remove render from moq-video's defaults. Keep nvidia and mediacodec
defaults and the existing opt-in system-library/libclang features. Remove the
nvenc/nvdec compatibility aliases in video and transcode before 0.1.

Forward the software-backend choice through transcode and explicitly select
features in workspace consumers, tests, and examples. A hardware-only build
without a usable codec refuses construction; Software and Named selection must
never silently use another class of backend. No mandatory toolchain is added
to moq-nvenc, whose bindings are checked in and drivers are loaded at runtime.

Audio keeps its pure-Rust AAC default. Make PipeWire/PulseAudio host flags avoid
activating cpal when neither capture nor playback needs it, and document the
capability combination. Do not split encode/decode into more features without
evidence of useful savings.

Add independent no-default, software-only, native-only, and rendering feature
checks for the affected crates to CI. Inspect dependency graphs to prove
OpenH264/wgpu/cpal are absent when excluded; workspace-wide builds are not
that proof. Preserve codec-independent tests when a backend is disabled.

Public API: Cargo defaults and supported feature names change. Wire: none.
Update feature tables and build prerequisites with the same change.
