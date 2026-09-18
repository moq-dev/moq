# [M] OBS exits cleanly with the moq plugin loaded

## Goal

OBS exits without a crash whether a moq output is running, was just stopped,
or a source still has terminal callbacks outstanding. libmoq parks a
process-wide `libmoq` thread in `block_on` on first use and has no way to
stop it, while OBS `dlclose`s each plugin at shutdown; the plugin links
libmoq statically, so the thread's code is unmapped under it. Any host that
unloads the library has the same exposure.

## Plan

Starts on `main`; libmoq's runtime (`rs/libmoq/src/ffi.rs`) is the same on
both branches and the change is additive.

- Reproduce first: exit OBS with the plugin loaded and an output running,
  then again right after stopping it, and again with a source whose
  `moq_source_destroy` hit its two-second backstop. Record which of these
  crashes, and how, in the PR before changing anything.
- Add `moq_shutdown()` to the C ABI, mirroring `moq_ffi_shutdown` in
  `rs/moq-ffi`: it stops and joins the `libmoq` thread. Pending calls resolve
  as cancelled, the terminal callback for every still-open handle fires with
  a cancelled status before it returns, and any later call returns an error
  code rather than restarting the runtime. It is idempotent and must be
  called from a host thread, never from a libmoq callback. Native codec and
  capture threads belong to their handles and end with them; this call does
  not reach into them.
- Wire it: define `obs_module_unload` in `cpp/obs/src/obs-moq.cpp` and call
  `moq_shutdown()` there, after OBS has destroyed the outputs and sources.
  Add the corresponding `MOQ_LOG` line so a hung shutdown is visible.
- Regression tests: a `rs/libmoq/c-tests` fixture that builds libmoq as a
  cdylib, `dlopen`s it, opens a session, and `dlclose`s once without
  `moq_shutdown` (must fail, proving the hazard) and once with it (clean
  exit, thread gone); a `rs/libmoq` unit test that pending and later calls
  resolve cancelled and open handles close without panicking afterwards
  (mirror `rs/moq-ffi/src/test.rs::shutdown_cancels_and_drops_cleanly`,
  in a child process since the stop is process-wide); and a `cpp/obs/test`
  stub test that `obs_module_unload` calls it. Wire the new fixture into
  `just rs c-tests`.
- Cross-package sync: `moq.h` is cbindgen-generated from the Rust doc
  comment; update `doc/lib/c` (the handle and callback contract) and
  `doc/bin/obs.md`. The Go bindings regenerate from `moq.h` and need no
  wrapper: `os.Exit` ends the process without tearing a host runtime down
  under the thread.

Public API: additive on the libmoq C ABI (`moq_shutdown`). Wire: none.

## Related

- [Kotlin JVM exit](/quest/m2/kt-jvm-exit.md) - the same hazard class for the moq-ffi bindings
