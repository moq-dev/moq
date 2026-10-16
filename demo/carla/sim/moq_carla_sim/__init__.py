"""Simulated trucks in one shared CARLA world, published over MoQ.

The trucks drive themselves under Traffic Manager and can be taken over from
the browser. Every driver command passes the safety gate in `drive` before it
reaches CARLA.

Python rather than Rust because the `carla` crate's C++ bindings (`carla-sys`,
via autocxx) only build against LLVM 11-13, which neither nixpkgs nor Fedora
still ship. CARLA's own client is a Python wheel, and moq's Python bindings
cover publishing and the JSON control tracks, so the whole worker fits in one
process with no FFI of ours in the middle.
"""
