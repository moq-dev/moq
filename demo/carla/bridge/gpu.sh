#!/usr/bin/env bash
# Run a command against this host's NVIDIA driver and Vulkan loader, or skip on
# a host with no NVIDIA GPU. The GPU is found on the PCI bus rather than by its
# libraries, because the userspace libraries outlive a removed card. A card
# without a loaded driver is an error, so a host that should run the command
# never skips. CARLA_GPU_ROOT prefixes every probed path for the test.
set -euo pipefail

root="${CARLA_GPU_ROOT:-}"

gpu=
shopt -s nullglob
for device in "$root"/sys/bus/pci/devices/*; do
    # NVIDIA's vendor ID with a display controller class (VGA, 3D, ...).
    if [[ "$(<"$device/vendor")" == 0x10de && "$(<"$device/class")" == 0x03* ]]; then
        gpu="${device##*/}"
        break
    fi
done
shopt -u nullglob

if [[ -z "$gpu" ]]; then
    echo "bridge-gpu: skipped, no NVIDIA GPU on this host" >&2
    exit 0
fi

if [[ ! -d "$root/proc/driver/nvidia" ]]; then
    echo "bridge-gpu: NVIDIA GPU $gpu has no loaded kernel driver (no /proc/driver/nvidia); install the NVIDIA driver or load the nvidia module" >&2
    exit 1
fi

# NixOS keeps the loader outside the driver's directory, so they are found separately.
driver=
for candidate in "$root/run/opengl-driver/lib" "$root/usr/lib64" "$root/usr/lib/x86_64-linux-gnu" "$root/usr/lib"; do
    if [[ -e "$candidate/libcuda.so.1" && -e "$candidate/libnvidia-encode.so.1" ]]; then
        driver="$candidate"
        break
    fi
done
if [[ -z "$driver" ]]; then
    echo "bridge-gpu: NVIDIA GPU $gpu has no driver libraries with CUDA and NVENC (libcuda.so.1, libnvidia-encode.so.1); install the NVIDIA driver's userspace" >&2
    exit 1
fi

vulkan=
for candidate in "$driver" "$root/run/current-system/sw/lib" "$root/usr/lib64" "$root/usr/lib/x86_64-linux-gnu" "$root/usr/lib"; do
    if [[ -e "$candidate/libvulkan.so.1" ]]; then
        vulkan="$candidate"
        break
    fi
done
if [[ -z "$vulkan" ]]; then
    echo "bridge-gpu: the NVIDIA driver is in $driver but no Vulkan loader (libvulkan.so.1) was found; install one" >&2
    exit 1
fi

LD_LIBRARY_PATH="$driver:$vulkan${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" exec "$@"
