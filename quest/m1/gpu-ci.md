# [S] NVIDIA tests run on real hardware

## Goal

The NVDEC, NVENC, and CUDA tests run nightly on the maintainer's Linux host
(RTX 3070 Ti) instead of passing without a GPU on hosted runners, and a local
`just rs nvidia` runs them against the host driver instead of silently
skipping inside the Nix shell.

## Plan

- Why they skip: the driver libraries are loaded at runtime (`libcuda` by
  cudarc, `libnvidia-encode` in `rs/moq-nvenc/src/safe/api.rs`, `libnvcuvid`
  in `rs/moq-nvenc/src/cuvid.rs`), and the tests return early when they are
  missing (`hw_available` in `rs/moq-video/src/decode/backend/nvdec.rs`). The
  Nix shell's loader path lacks Ubuntu's `/usr/lib/x86_64-linux-gnu`.
- `just rs nvidia`: symlink only those three libraries (by soname) from
  `/usr/lib/x86_64-linux-gnu` into a private directory, put that on
  `LD_LIBRARY_PATH`, and run `cargo nextest run -E 'test(/nvdec|nvenc|cuda/)'`.
  Fail when a library is missing instead of skipping. `just rs vulkan-cuda`
  puts the whole host directory on the path, which lets host libraries shadow
  the Nix ones; move it onto the same directory, and decide whether the
  nightly also runs its ignored `vulkan_cuda_` tests.
- Nightly: a job in `.github/workflows/nightly.yml` runs `just rs nvidia` on
  the self-hosted runner. A self-hosted runner on a public repository must
  never run untrusted code: only `schedule` and `workflow_dispatch`, with the
  job gated to `refs/heads/main`, never `pull_request`; a dedicated label only
  this job selects; read-only `permissions`. Read GitHub's self-hosted runner
  hardening guidance before wiring it.
- Share the runner with the io_uring one that #4132 plans
  (`quest/m1/uring-runner.md` on the drain line, which wants a 6.12+ kernel on
  the same host): one registration and one security posture, a label per
  capability. Whichever quest lands second reuses the first's job shape.

Public API: none. Wire: none.

## Required

- A self-hosted runner is registered for moq-dev/moq on the maintainer's host, with the NVIDIA driver

## Related

- [NVDEC teardown](/quest/m1/nvdec-teardown.md) - the kind of GPU-only crash this job catches
- [Video hardware validation](/quest/m3/video-hardware.md) - hardware paths nothing runs yet
- [Runtime QA hosts](/quest/m2/runtime-qa-hosts.md) - on-demand jobs on hardware hosts, a broader contract than a nightly
