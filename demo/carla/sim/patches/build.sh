#!/usr/bin/env bash
# Build the CARLA simulator image the worker needs from source: clean CARLA plus
# rpc-idle-wait.patch and the GPU bridge camera, on the engine plus its
# timeline-semaphore patch, compiled in a rootless Ubuntu 22.04 container, with
# the bridge library beside it, overlaid onto the published image. Prints the
# local tag to pin as CARLA_IMAGE.
#
# Needs the Epic-gated CarlaUnreal/UnrealEngine repo through `gh`, and about
# 40 GB. Re-running resumes: sources, the engine's dependency fetch, and
# every compiled step are kept in the workspace, which is deliberately not /tmp.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# The repository root, mounted read-only into both build containers: the bridge
# crate is a workspace member, and compile.sh reads the plugin and its header.
repo="$(cd "$here/../../../.." && pwd)"
work="${CARLA_BUILD:-$HOME/.cache/moq-carla-build}"

carla_rev=294096eb1c38eabf246e4f3a9cdab704e33a7f4c
carla_release=0.9.16
engine_rev=e9d9e60c85f643e10eeb03f42f61554d18dcb30f
builder=localhost/moq-carla-builder:22.04
# The bridge library runs inside the published image, Ubuntu 20.04 with glibc
# 2.31, so it is built on Debian bullseye, which has the same glibc, with the
# image's own toolchain rather than the repository's patch-level pin. Not the
# slim image: the workspace unifies openh264 into moq-video, which needs g++.
rust=docker.io/library/rust:1.98.0-bullseye@sha256:58643a517df55089a44540d827eb9bfd8d5f121c249767be6e25f24b68c0563d

# The build may share its host with a running simulator. It runs at idle CPU and
# I/O priority, and the memory cap makes an out-of-memory kill land in the build.
ninja_jobs="${NINJA_JOBS:-12}"
ue_jobs="${UE_JOBS:-10}"
memory="${BUILD_MEMORY:-36g}"

mkdir -p "$work/logs" "$work/home" "$work/bridge" "$work/out"

bash "$here/checkout.sh" "$work/carla" https://github.com/carla-simulator/carla.git "$carla_rev"
# CARLA names its version after a `ue4/<release>` branch and otherwise after the
# short hash, which clients refuse as a version mismatch. Local edits survive.
git -C "$work/carla" checkout -q -B "ue4/$carla_release"
# Epic-gated: GitHub serves it only to accounts linked to an Epic account.
bash "$here/checkout.sh" "$work/engine" https://github.com/CarlaUnreal/UnrealEngine.git "$engine_rev" \
    -c credential.helper= -c 'credential.helper=!gh auth git-credential'

podman build -q -t "$builder" -f "$here/builder.Containerfile" "$here" >/dev/null

log="$work/logs/compile-$(date -u +%Y%m%dT%H%M%SZ).log"
echo "build: compiling the bridge library, log in $log" >&2
podman run --rm --name moq-carla-bridge-build \
    --userns=keep-id --security-opt label=disable --memory="$memory" \
    -e CARGO_HOME=/work/bridge/cargo -e CARGO_TARGET_DIR=/work/bridge/target \
    -e RUSTUP_TOOLCHAIN=1.98.0 -e CARGO_BUILD_JOBS="$ninja_jobs" \
    -v "$work:/work" -v "$repo:/repo:ro" -w /repo \
    "$rust" nice -n 19 cargo build --locked --release -p moq-carla-bridge 2>&1 | tee "$log" >&2
cp "$work/bridge/target/release/libcarla_bridge.so" "$work/out/"

echo "build: compiling CARLA, log in $log" >&2
# label=disable: relabeling the workspace for SELinux would walk tens of GB.
podman run --rm --name moq-carla-build \
    --userns=keep-id --security-opt label=disable --memory="$memory" \
    -e HOME=/work/home -e CARLA_RELEASE="$carla_release" \
    -e NINJA_JOBS="$ninja_jobs" -e UE_JOBS="$ue_jobs" \
    -v "$work:/work" -v "$repo:/repo:ro" \
    "$builder" nice -n 19 ionice -c3 bash /repo/demo/carla/sim/patches/compile.sh 2>&1 | tee -a "$log" >&2

sha="$(sha256sum "$work/out/CarlaUE4-Linux-Shipping" | cut -d' ' -f1)"
bridge_sha="$(sha256sum "$work/out/libcarla_bridge.so" | cut -d' ' -f1)"
tag="localhost/moq-carla:$carla_release-bridge-${sha:0:12}-${bridge_sha:0:12}"
podman build -q -t "$tag" --label "moq.carla.shipping-sha256=$sha" \
    --label "moq.carla.bridge-sha256=$bridge_sha" \
    -f "$here/runtime.Containerfile" "$work/out" >/dev/null
echo "build: $tag (CarlaUE4-Linux-Shipping sha256 $sha, libcarla_bridge.so sha256 $bridge_sha)" >&2
echo "$tag"
