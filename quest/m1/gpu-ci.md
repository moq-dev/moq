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
- Select every test that needs the GPU, not only names containing `nvdec`,
  `nvenc`, or `cuda`: `safe::session::tests::failed_submission_releases_the_session`
  and `safe::session::tests::start_session_refuses_held_frames` in `rs/moq-nvenc`
  need hardware and match none of them. Find them by
  their driver probes (`hw_available`, `driver_libs_present`, `Api::get` and
  friends in `moq-nvenc` and `moq-video`). Recommendation: follow the
  existing `#[ignore = "requires ..."]` convention (as `frame/vulkan_test.rs`
  does) and put them in `nvidia` test modules, so hosted CI reports them
  ignored instead of passed and one filter, `--run-ignored only -E
  'test(/::nvidia::/)'`, selects them all without the other ignored hardware
  tests (Android, D3D11, PipeWire). Inside that selection a missing GPU fails
  the test instead of returning early. Keep the no-driver tests
  (`missing_driver_errors_instead_of_panicking`) outside it.
- `just rs nvidia`, a one-line recipe over `sh/rs/nvidia.sh`: symlink only those three libraries (by
  soname) from `/usr/lib/x86_64-linux-gnu` into a private directory, put that
  on `LD_LIBRARY_PATH`, and run that selection. Fail when a library is missing
  instead of skipping. `sh/rs/vulkan-cuda.sh`, the NVIDIA branch of
  `just rs gpu`, already symlinks those three libraries for its `vulkan_cuda_`
  tests; widen it into this script rather than adding a second one. They also need
  the Vulkan loader to find the host NVIDIA ICD: point it at the ICD manifest
  and expose the driver libraries it names, or keep them in their own recipe.
- `just rs gpu` (added by #4975) detects the host's GPU vendors and runs
  each one's ignored tests. `just rs nvidia` is its NVIDIA branch, not a
  second detector.
- The selection includes the `vulkan_cuda_` external-image tests in
  `rs/moq-video/src/frame/cuda_test.rs`, which have never run on hardware.
  `vulkan_cuda_auto_external_encode` must pass its per-plane pixel checks
  against the CPU reference for both renditions, RGBA and BGRA, through
  NVENC and CUDA external-image import (the verification #4975 deferred here).
- Nightly: a job in `.github/workflows/nightly.yml` runs `nix develop
  --command just rs nvidia` on the self-hosted runner, a recipe and not a
  script path, like every other workflow step. A self-hosted runner on a public repository must
  never run untrusted code: only `schedule` and `workflow_dispatch`, with the
  job gated to `refs/heads/main`, never `pull_request`; a dedicated label only
  this job selects; read-only `permissions`. Read GitHub's self-hosted runner
  hardening guidance before wiring it.

Public API: none. Wire: none.

## Required

- [A self-hosted NVIDIA runner is registered](/quest/m1/gpu-runner.md) - the host the nightly job runs on

## Related

- [Video hardware validation](/quest/m3/video-hardware.md) - hardware paths nothing runs yet
