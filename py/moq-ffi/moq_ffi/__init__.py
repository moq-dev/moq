"""Raw UniFFI bindings for the Media over QUIC Rust crates.

This package exposes the auto-generated bindings exactly as uniffi-bindgen
emits them (the `Moq`-prefixed classes). It is the native foundation that the
ergonomic `moq` wrapper builds on. Most callers want `moq`, not this.

The compiled cdylib plus generated bindings live in the private `_uniffi`
submodule; everything public is re-exported here.
"""

from ._uniffi import *  # noqa: F401,F403
