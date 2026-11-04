#!/usr/bin/env bash
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true && "${LOCAL_SUPERVISED:-}" != 1 ]]; then
    exec bash "$(dirname "$0")/../local.sh" bash "$0" "$@"
fi

all=0
if [[ $# == 1 && $1 == --all ]]; then
    all=1
elif [[ $# != 0 ]]; then
    echo 'usage: just rs media-features [--all]' >&2
    exit 2
fi

targets=(--tests)
if ((all)); then targets=(--all-targets); fi

check() {
    echo "media features: cargo check $*"
    cargo check --locked "${targets[@]}" "$@"
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

# Minimal shapes and the off-default VPX backend always compile and test, and
# the dependency absence contracts always run. --all, which `just rs features`
# passes, also compiles each positive path in isolation.
check -p moq-video --no-default-features
minimal=$(tree -p moq-video --no-default-features)
for crate in openh264 openh264-sys2 wgpu cpal; do
    forbid_crate "$minimal" "$crate" "moq-video minimal"
done
cargo nextest run --locked -p moq-video --no-default-features

if ((all)); then check -p moq-video --no-default-features --features openh264; fi
software=$(tree -p moq-video --no-default-features --features openh264)
require_crate "$software" openh264 "moq-video software-only"
require_crate "$software" openh264-sys2 "moq-video software-only"
forbid_crate "$software" wgpu "moq-video software-only"

if ((all)); then check -p moq-video --no-default-features --features nvidia; fi
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

if ((all)); then check -p moq-video --no-default-features --features render; fi
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

if ((all)); then check -p moq-transcode --no-default-features --features openh264; fi
transcode_software=$(tree -p moq-transcode --no-default-features --features openh264)
require_crate "$transcode_software" openh264 "moq-transcode software-only"
forbid_crate "$transcode_software" wgpu "moq-transcode software-only"

if ((all)); then check -p moq-transcode --no-default-features --features nvidia; fi
transcode_native=$(tree -p moq-transcode --no-default-features --features nvidia)
require_crate "$transcode_native" moq-nvenc "moq-transcode native-only"
for crate in openh264 openh264-sys2 wgpu; do
    forbid_crate "$transcode_native" "$crate" "moq-transcode native-only"
done

check -p moq-audio --no-default-features --features pipewire
audio_host=$(tree -p moq-audio --no-default-features --features pipewire)
forbid_crate "$audio_host" cpal "moq-audio host-only"

if ((all)); then check -p moq-audio --no-default-features --features capture,pipewire; fi
audio_capture=$(tree -p moq-audio --no-default-features --features capture,pipewire)
require_crate "$audio_capture" cpal "moq-audio PipeWire capture"

echo "media features: dependency contracts ok"
