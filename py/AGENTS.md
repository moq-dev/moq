The `/py` uv workspace. Run recipes via `just py <recipe>`. See also `rs/moq-ffi/AGENTS.md`.

- `moq-ffi/` (`import moq_ffi`) is the generated uniffi layer: re-exports only, no hand-written ergonomics. `moq-rs/` (`import moq`) is the ergonomic wrapper.
- Each layer above the moq-net root is a submodule (`moq.json`) with its own `__all__`, named after the Rust crate it mirrors.
- `Client`/`Server` are async context managers; iterate announcements and tracks with `async for`.
- Extend with keyword-only args with defaults (`*, ...`), not options objects.
- The package ships `py.typed`: document public symbols.
