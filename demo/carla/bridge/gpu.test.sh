#!/usr/bin/env bash
# Offline regression for gpu.sh's NVIDIA detection against a fake host root.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

pci() {
    mkdir -p "$tmp/root/sys/bus/pci/devices/$1"
    echo "$2" >"$tmp/root/sys/bus/pci/devices/$1/vendor"
    echo "$3" >"$tmp/root/sys/bus/pci/devices/$1/class"
}

# Runs gpu.sh against the fake root; the command prints the loader path it got.
run() {
    CARLA_GPU_ROOT="$tmp/root" LD_LIBRARY_PATH='' bash "$here/gpu.sh" bash -c 'echo "ran $LD_LIBRARY_PATH"' >"$tmp/out" 2>&1
}

expect_skip() {
    run || {
        cat "$tmp/out" >&2
        echo "gpu.sh failed, expected a skip: $1" >&2
        exit 1
    }
    if ! grep -Fq 'skipped, no NVIDIA GPU' "$tmp/out" || grep -Fq ran "$tmp/out"; then
        cat "$tmp/out" >&2
        echo "gpu.sh did not skip: $1" >&2
        exit 1
    fi
}

expect_error() {
    if run; then
        cat "$tmp/out" >&2
        echo "gpu.sh succeeded, expected an error: $1" >&2
        exit 1
    fi
    grep -Fq "$2" "$tmp/out"
}

# No /sys at all, like a Mac.
mkdir -p "$tmp/root"
expect_skip "no sysfs"

# Leftover NVIDIA libraries with only Intel and AMD GPUs, plus an NVIDIA
# non-display function (an audio controller), must still skip.
mkdir -p "$tmp/root/usr/lib/x86_64-linux-gnu"
touch "$tmp/root/usr/lib/x86_64-linux-gnu/"{libcuda.so.1,libnvidia-encode.so.1,libvulkan.so.1}
pci 0000:00:02.0 0x8086 0x030000
pci 0000:04:00.0 0x1002 0x030000
pci 0000:04:00.1 0x10de 0x040300
expect_skip "stale libraries without an NVIDIA GPU"

# An NVIDIA GPU without the kernel driver loaded.
pci 0000:01:00.0 0x10de 0x030000
expect_error "no kernel driver" "no loaded kernel driver"

# The kernel driver without the userspace libraries.
mkdir -p "$tmp/root/proc/driver/nvidia"
rm "$tmp/root/usr/lib/x86_64-linux-gnu/"{libcuda.so.1,libnvidia-encode.so.1}
expect_error "no driver libraries" "no driver libraries"

# The driver without a Vulkan loader.
touch "$tmp/root/usr/lib/x86_64-linux-gnu/"{libcuda.so.1,libnvidia-encode.so.1}
rm "$tmp/root/usr/lib/x86_64-linux-gnu/libvulkan.so.1"
expect_error "no Vulkan loader" "no Vulkan loader"

# NixOS: the driver and loader live in separate directories.
mkdir -p "$tmp/root/run/opengl-driver/lib" "$tmp/root/run/current-system/sw/lib"
touch "$tmp/root/run/opengl-driver/lib/"{libcuda.so.1,libnvidia-encode.so.1}
touch "$tmp/root/run/current-system/sw/lib/libvulkan.so.1"
run
grep -Fxq "ran $tmp/root/run/opengl-driver/lib:$tmp/root/run/current-system/sw/lib" "$tmp/out"

printf 'bridge GPU detection test: ok\n'
