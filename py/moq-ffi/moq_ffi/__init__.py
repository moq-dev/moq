"""Raw UniFFI bindings for the Media over QUIC Rust crates.

This package exposes the auto-generated bindings exactly as uniffi-bindgen
emits them (the `Moq`-prefixed classes). It is the native foundation that the
ergonomic `moq` wrapper builds on. Most callers want `moq`, not this.

The compiled cdylib plus generated bindings live in the private `_uniffi`
submodule; everything public is re-exported here.
"""

import atexit

from ._uniffi import *  # noqa: F401,F403
from ._uniffi.moq import _UniffiLib

# The Rust runtime thread resumes every awaiting coroutine through a ctypes
# callback, and CPython (before 3.14) kills a foreign thread that touches the
# interpreter once finalization has begun: pthread_exit, whose unwind aborts in
# Rust. atexit runs before finalization, after the program's own handlers, so
# stop the thread here. Calls after this point resolve as cancelled.
_UniffiLib.moq_ffi_shutdown.argtypes = ()
_UniffiLib.moq_ffi_shutdown.restype = None
atexit.register(_UniffiLib.moq_ffi_shutdown)
