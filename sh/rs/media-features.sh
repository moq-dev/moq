#!/usr/bin/env bash
set -euo pipefail

check() {
    echo "media features: cargo check $*"
    cargo check --locked --all-targets "$@"
}

# Resolved for Linux, the platform every asserted dependency targets: moq-nvenc
# and cpal's PipeWire host are Linux-only, so a host-native graph would fail
# the proof on macOS or Windows.
tree() {
    cargo tree --locked --prefix none -e normal --target x86_64-unknown-linux-gnu "$@"
}

require_crate() {
    local graph=$1 crate=$2 label=$3
    if ! grep -Eq "^${crate} v" <<<"$graph"; then
        echo "media features: $label must contain $crate" >&2
        exit 1
    fi
}

forbid_crate() {
    local graph=$1 crate=$2 label=$3
    if grep -Eq "^${crate} v" <<<"$graph"; then
        echo "media features: $label unexpectedly contains $crate" >&2
        exit 1
    fi
}

# Each shape is compiled independently so workspace feature unification cannot
# hide a missing gate. The normal dependency graph then proves the expensive
# stacks are absent rather than merely unused by this target.
check -p moq-video --no-default-features
minimal=$(tree -p moq-video --no-default-features)
for crate in openh264 openh264-sys2 wgpu cpal; do
    forbid_crate "$minimal" "$crate" "moq-video minimal"
done
cargo nextest run --locked -p moq-video --no-default-features

check -p moq-video --no-default-features --features openh264
software=$(tree -p moq-video --no-default-features --features openh264)
require_crate "$software" openh264 "moq-video software-only"
require_crate "$software" openh264-sys2 "moq-video software-only"
forbid_crate "$software" wgpu "moq-video software-only"

check -p moq-video --no-default-features --features nvidia
native=$(tree -p moq-video --no-default-features --features nvidia)
require_crate "$native" moq-nvenc "moq-video native-only"
for crate in openh264 openh264-sys2 wgpu; do
    forbid_crate "$native" "$crate" "moq-video native-only"
done

# libvpx comes from the build host (the Nix dev shell here), so the default
# check never compiles this backend. Its tests decode committed fixtures and
# need no hardware, so they run here rather than waiting for the nightly.
check -p moq-video --no-default-features --features vpx
vpx=$(tree -p moq-video --no-default-features --features vpx)
require_crate "$vpx" libvpx-native-sys "moq-video VP8/VP9"
for crate in openh264 openh264-sys2 wgpu; do
    forbid_crate "$vpx" "$crate" "moq-video VP8/VP9"
done
cargo nextest run --locked -p moq-video --no-default-features --features vpx -E 'test(/::vpx::/)'

check -p moq-video --no-default-features --features render
render=$(tree -p moq-video --no-default-features --features render)
require_crate "$render" wgpu "moq-video rendering"
for crate in openh264 openh264-sys2; do
    forbid_crate "$render" "$crate" "moq-video rendering"
done

check -p moq-transcode --no-default-features
transcode_minimal=$(tree -p moq-transcode --no-default-features)
for crate in openh264 openh264-sys2 wgpu moq-nvenc; do
    forbid_crate "$transcode_minimal" "$crate" "moq-transcode minimal"
done

check -p moq-transcode --no-default-features --features openh264
transcode_software=$(tree -p moq-transcode --no-default-features --features openh264)
require_crate "$transcode_software" openh264 "moq-transcode software-only"
forbid_crate "$transcode_software" wgpu "moq-transcode software-only"

check -p moq-transcode --no-default-features --features nvidia
transcode_native=$(tree -p moq-transcode --no-default-features --features nvidia)
require_crate "$transcode_native" moq-nvenc "moq-transcode native-only"
for crate in openh264 openh264-sys2 wgpu; do
    forbid_crate "$transcode_native" "$crate" "moq-transcode native-only"
done

check -p moq-audio --no-default-features --features pipewire
audio_host=$(tree -p moq-audio --no-default-features --features pipewire)
forbid_crate "$audio_host" cpal "moq-audio host-only"

check -p moq-audio --no-default-features --features capture,pipewire
audio_capture=$(tree -p moq-audio --no-default-features --features capture,pipewire)
require_crate "$audio_capture" cpal "moq-audio PipeWire capture"

echo "media features: dependency contracts ok"
