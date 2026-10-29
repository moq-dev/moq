# [M] NVIDIA tests gate the merge queue on real hardware

## Goal

The NVDEC, NVENC, and CUDA tests gate the merge queue on real NVIDIA
hardware instead of passing without a GPU on hosted runners, and a local
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
- Merge queue: a `merge_group` job in `.github/workflows/check.yml` (or
  `video.yml`) runs `nix develop --command just rs nvidia`, a recipe and not
  a script path, like every other workflow step, on a self-hosted runner VM
  labeled for the NVIDIA host. Scope it like the other queue jobs: run when
  the queue's diff touches moq-video, moq-nvenc, or what they depend on.
  Read-only `permissions`, never `pull_request`.
- Decided 2026-10-20 (maintainer): GPU tests gate in the merge queue on the
  NVIDIA card, because they gate correctness, not trends. The runner is a
  persistent VM from the `github-runner-vm` module (#6040,
  `nix/runner/README.md`). That module has no GPU passthrough today: add
  VFIO passthrough of the NVIDIA card to the guest, with the driver in the
  guest, and keep the runner registered only to this repository with
  approval required for every external contributor's workflow, as the
  README requires.
- Open (2026-10-25): the [CI epic](/quest/m0/ci/README.md) dropped the VM
  for the CI host in favor of kernel sandboxing, which conflicts with VFIO
  passthrough. Recommendation: a container runner with the NVIDIA device
  nodes and the host driver's libraries bound in. Confirm with the
  maintainer before starting.

Public API: none. Wire: none.

## Related

- [Video hardware validation](/quest/m3/video-hardware.md) - hardware paths nothing runs yet
- [Video quality hardware rows](/quest/m2/video-quality/hardware.md) - the nightly quality job that also needs this runner's GPU
