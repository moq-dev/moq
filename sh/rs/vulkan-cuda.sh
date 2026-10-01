#!/usr/bin/env bash
# Exercise the Vulkan producer -> CUDA image contract, the GPU color conversion
# and resize, and NVENC encoding of the result on Linux/NVIDIA hardware. The
# driver directory is added explicitly because the Nix shell's dynamic loader
# path does not include Ubuntu's host driver directory.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

driver=$(/usr/sbin/ldconfig -p | awk '/libcuda\.so\.1/{print $NF; exit}')
if [[ -z "$driver" ]]; then
    echo "libcuda.so.1 is not installed" >&2
    exit 1
fi
driver_dir=$(dirname "$driver")
export LD_LIBRARY_PATH="$driver_dir${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
cargo nextest run --locked -p moq-video --run-ignored only -E 'test(/^frame::(vulkan|cuda)::tests::vulkan_cuda_/)'
