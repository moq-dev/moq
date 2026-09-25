#!/bin/sh
# Regenerates videodev2.rs from the host's kernel headers. Run on Linux with
# bindgen-cli and the kernel UAPI headers installed (linux-libc-dev on Debian);
# the output is arch-independent, so any 64-bit host produces the same file.
set -eu
cd "$(dirname "$0")"
echo '#include <linux/videodev2.h>' >wrapper.h
trap 'rm -f wrapper.h' EXIT
bindgen wrapper.h \
    --no-layout-tests \
    --allowlist-type 'v4l2.*' \
    --allowlist-var 'V4L2_.*' \
    --blocklist-type '(timeval|timespec|__time_t|__suseconds_t|__syscall_slong_t)' \
    --raw-line 'pub use libc::{timespec, timeval};' \
    -o videodev2.rs
