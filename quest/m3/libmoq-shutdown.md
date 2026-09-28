# [M] libmoq can be stopped before its host unloads it

## Goal

A host that unloads libmoq can stop it first. libmoq parks a process-wide
`libmoq` thread in `block_on` on first use and has no way to stop it, so a
host that `dlclose`s the library, or a plugin that links it statically and is
unloaded by its host, unmaps code that thread is still running and crashes on
exit. moq-ffi already has `moq_ffi_shutdown` for this; the C ABI needs the same.

## Plan
Parked in m3 behind [Generated C bindings](/quest/m1/c/README.md): the hand-written libmoq is being replaced by C generated from moq-ffi, which carries this for free. Do it only if the hand-written crate outlives that line.


Starts on `main`; libmoq's runtime (`rs/libmoq/src/ffi.rs`) is the same on
both branches and the change is additive.

- Reproduce first with a fixture that `dlopen`s libmoq, opens a session, and
  `dlclose`s it, and record how it fails in the PR before changing anything.
- Add `moq_shutdown()` to the C ABI, mirroring `moq_ffi_shutdown` in
  `rs/moq-ffi`: it stops and joins the `libmoq` thread. Pending calls resolve
  as cancelled, the terminal callback for every still-open handle fires with
  a cancelled status before it returns, and any later call returns an error
  code rather than restarting the runtime. It is idempotent and must be
  called from a host thread, never from a libmoq callback. Native codec and
  capture threads belong to their handles and end with them; this call does
  not reach into them.
- Regression tests: a `rs/libmoq/c-tests` fixture that builds libmoq as a
  cdylib, `dlopen`s it, opens a session, and `dlclose`s once without
  `moq_shutdown` (must fail, proving the hazard) and once with it (clean
  exit, thread gone); a `rs/libmoq` unit test that pending and later calls
  resolve cancelled and open handles close without panicking afterwards
  (mirror `rs/moq-ffi/src/test.rs::shutdown_cancels_and_drops_cleanly`,
  in a child process since the stop is process-wide). Wire the new fixture
  into `just rs c-tests`.
- Cross-package sync: `moq.h` is cbindgen-generated from the Rust doc
  comment; update `doc/lib/c` (the handle and callback contract) and
  the Go bindings regenerate from `moq.h` and need no
  wrapper: `os.Exit` ends the process without tearing a host runtime down
  under the thread.

Public API: additive on the libmoq C ABI (`moq_shutdown`). Wire: none.

## Related

- [C++ through moq-ffi](/quest/m1/cpp/README.md) - moves the OBS plugin, the host that found this, onto moq-ffi, whose unload calls `moq::shutdown()`
- [Kotlin JVM exit](/quest/m1/kt-jvm-exit.md) - the same hazard class for the moq-ffi bindings
