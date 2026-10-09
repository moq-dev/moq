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
# Expose only NVIDIA libraries; adding the whole host directory can override
# the devshell's newer libc and libm with incompatible host copies.
driver_dir=$(mktemp -d)
trap 'rm -rf "$driver_dir"' EXIT
for library in libcuda.so.1 libnvidia-encode.so.1 libnvidia-ptxjitcompiler.so.1; do
    path=$(/usr/sbin/ldconfig -p | awk -v name="$library" '$1 == name {print $NF; exit}')
    [[ -n "$path" ]] || {
        echo "$library is not installed" >&2
        exit 1
    }
    ln -s "$path" "$driver_dir/$library"
done
export LD_LIBRARY_PATH="$driver_dir${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
just rs test -p moq-video --run-ignored only -E 'test(/^frame::(vulkan|cuda)::tests::vulkan_cuda_/)'
