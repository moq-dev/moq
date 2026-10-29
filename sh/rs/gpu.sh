#!/usr/bin/env bash
# Hardware tests are opt-in; a detected GPU must have its driver and pass its tests.
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true && "${LOCAL_SUPERVISED:-}" != 1 ]]; then
    exec bash "$(dirname "$0")/../local.sh" bash "$0" "$@"
fi
cd "$(git rev-parse --show-toplevel)"

declare -A vendors=()
for device in /sys/bus/pci/devices/*; do
    [[ -f "$device/class" && $(<"$device/class") == 0x03* ]] || continue
    vendor=$(<"$device/vendor")
    case "$vendor" in
        0x10de | 0x1002 | 0x8086) ;;
        *)
            # A BMC or virtual display adapter has no tests; a supported GPU still must.
            echo "Skipping unsupported display controller $vendor at $device" >&2
            continue
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
    case "$vendor" in
        0x10de)
            MOQ_GPU_VENDOR="$vendor" just rs test -p moq-video --run-ignored only -E 'test(vulkan_gpu_device)'
            sh/rs/vulkan-cuda.sh
            ;;
        0x1002 | 0x8086)
            # The Nix shell has no Vulkan loader, and the host's Mesa links the host's
            # libc, so drive the GPU through the flake's own Mesa, as `just rs render-sw` does.
            mesa=$(nix build --inputs-from . --no-link --print-out-paths nixpkgs#mesa)
            loader=$(nix build --inputs-from . --no-link --print-out-paths nixpkgs#vulkan-loader)
            case "$vendor" in
                0x1002) icd=("$mesa"/share/vulkan/icd.d/radeon_icd.*.json) ;;
                0x8086) icd=("$mesa"/share/vulkan/icd.d/intel_icd.*.json) ;;
            esac
            LD_LIBRARY_PATH="$loader/lib" VK_DRIVER_FILES="${icd[0]}" MOQ_GPU_VENDOR="$vendor" \
                just rs test -p moq-video --run-ignored only -E 'test(vulkan_gpu_device)'
            # A real driver exports memory where Mesa's software one cannot, so an
            # encoder that cannot read the export is passed over after it opens.
            LD_LIBRARY_PATH="$loader/lib" VK_DRIVER_FILES="${icd[0]}" \
                just rs test -p moq-video --features render,openh264 --lib --run-ignored only \
                -E 'test(=render::export::vulkan::tests::vulkan_export_round_trips) | test(/^render::export::tests::(choosing_passes_over|a_named_encoder)_/)'
            echo "GPU $vendor: Vulkan driver verified; encoder hardware tests arrive with its backend"
            ;;
    esac
done
