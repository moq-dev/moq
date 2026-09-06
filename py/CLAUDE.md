The `/py` uv workspace. Extends the root `CLAUDE.md` and `rs/moq-ffi/CLAUDE.md`.

# Packages

- `moq-ffi/` (`import moq_ffi`): the generated uniffi layer over `rs/moq-ffi`, built with Maturin. One wheel covers everything moq-ffi exposes. Keep it thin: `__init__.py` re-exports generated symbols, no hand-written ergonomics.
- `moq-rs/` (`import moq`, since `moq` was taken on PyPI): the ergonomic wrapper consumers use.

# moq-rs

`moq/__init__.py` is the single public surface and defines `__all__`. Modules map to roles; keep names aligned with `rs/moq-net`.

# Conventions

- `Client`/`Server` are async context managers; iterate announcements and tracks with `async for`.
- Keyword-only args with defaults (`*, ...`) instead of an options object; that is how Python extends additively.
- The package ships `py.typed`: types and docstrings are the API, so document public symbols.
- `just py <recipe>` for everything. Tests live under each package's `tests/`.
- Releases: `moq-ffi` tracks `rs/moq-ffi` and publishes on `moq-ffi-v*` tags; `moq-rs` is versioned by hand and publishes on merge to `main` when the version isn't on PyPI yet.
