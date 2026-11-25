The `/py` uv workspace (`just py <recipe>`). See `rs/moq-ffi/AGENTS.md` and `quest context uniffi`.

- `moq-ffi/` (`import moq_ffi`) is the generated UniFFI layer (re-exports only); `moq-rs/` (`import moq`) is the ergonomic wrapper.
- Each layer above `moq-net` is a submodule (`moq.json`) with its own `__all__`, mirroring the Rust crate name.
