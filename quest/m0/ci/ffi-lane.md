# [M] Check the bindings in separate lanes

## Goal

A full queue run checks py, kt, swift, go, carla, and cpp concurrently, so
the binding checks leave the critical path. Each binding still validates
the default moq-ffi build its own `just <lang> check` would produce, and
those direct recipes stay self-contained.

## Plan

Decided with the maintainer on 2026-10-24: separate the shared state rather
than measure first or drop py's test features.

- In the full run of [moq-dev/moq#6617](https://github.com/moq-dev/moq/pull/6617)
  ([run 40118341261](https://github.com/moq-dev/moq/actions/runs/40118341261)),
  the serial `ffi` lane took 515s (py 265, cpp 123, kt 85, obs_compile 36),
  then `dart` ran alone for 109s: the whole 624s critical path.
- `sh/dispatch.sh` puts them in one lane because py builds
  `target/debug/libmoq_ffi.so` with its test features and the others read
  their default build from that path, carla's `uv sync` uninstalls py's
  editable build from the shared venv, and cpp and obs_compile share
  `cpp/moq/build`.
- Build moq-ffi's default cdylib once before the binding lanes start, and
  give py's test-featured build its own target directory so it never
  overwrites the shared one. Each recipe keeps its own build of the default
  cdylib, which then finds it fresh, so concurrent lanes never rebuild the
  shared output while another reads it. Give carla its own venv. Keep cpp and
  obs_compile in one lane. Every other binding becomes its own lane under
  `CHECK_JOBS`.
- dart keeps running alone: its build hook checks the filtered build stays
  fresh, which any concurrent Cargo build of moq-ffi would break.
- Measure the full-run wall time and peak disk before and after on matched
  inputs: concurrent lanes add their temporary data together.
