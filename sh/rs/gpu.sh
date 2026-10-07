#!/usr/bin/env bash
# Hardware tests are opt-in; a detected GPU must have its driver and pass its tests.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

declare -A vendors=()
for device in /sys/bus/pci/devices/*; do
    [[ -f "$device/class" && $(<"$device/class") == 0x03* ]] || continue
    vendor=$(<"$device/vendor")
    case "$vendor" in
        0x10de | 0x1002 | 0x8086) ;;
        *)
            echo "Unsupported GPU vendor $vendor at $device" >&2
            exit 1
            ;;
    esac
    [[ -L "$device/driver" ]] || {
        echo "GPU $device has no kernel driver" >&2
        exit 1
    }
    vendors[$vendor]=1
done
[[ ${#vendors[@]} -gt 0 ]] || {
    echo 'No supported PCI GPU detected' >&2
    exit 1
}

for vendor in "${!vendors[@]}"; do
    MOQ_GPU_VENDOR="$vendor" just rs test -p moq-video --run-ignored only -E 'test(vulkan_gpu_device)'
    case "$vendor" in
        0x10de) sh/rs/vulkan-cuda.sh ;;
        0x1002 | 0x8086) echo "GPU $vendor: Vulkan driver verified; encoder hardware tests arrive with its backend" ;;
    esac
done
