# py/CLAUDE.md

Scopes the `/py` uv workspace. Universal rules (writing style / no em-dashes, Branch Targeting, Cross-Package Sync, AI Attribution) live in the root `CLAUDE.md`. Detailed release/versioning mechanics for both distributions are in the root Project Structure notes; this file covers the code layout.

## Two packages, one wheel boundary

- `moq-ffi/` (`moq_ffi`): the generated uniffi bindings layer over `rs/moq-ffi`. A Maturin project; one wheel covers every crate exposed via moq-ffi (uniffi-linked libs cannot be split across wheels). Keep this layer thin. `moq_ffi/__init__.py` mostly re-exports generated symbols (`Container`, `MoqError`, `MoqSession`, `MoqClient`, ...). Do not hand-write ergonomics here; that belongs in `moq-rs`.
- `moq-rs/` (`moq`): the pure-python ergonomic wrapper consumers actually import (`import moq`). Depends on `moq-ffi` via a `~=0.2.x` compatible-release pin. This is where the friendly API lives.

## moq-rs layout

`moq/__init__.py` is the single public surface; it re-exports everything and defines `__all__`. Keep new public symbols flowing through it. Modules map to roles:

- `client.py` (`Client`): high-level connect with automatic origin wiring (simple mode) or a caller-provided origin (advanced mode).
- `server.py` (`Server`, `Request`, `Transport`): accept side.
- `origin.py`: `OriginProducer`/`OriginConsumer` and announce types, the pub/sub routing layer.
- `publish.py` / `subscribe.py`: the producer/consumer pairs (`Broadcast`, `Track`, `Group`, `Media`, `Audio`).
- `types.py`: plain data types (`Catalog`, `Frame`, `Video`, `Audio`, codecs, dimensions).

The wrapper mirrors the `rs/moq-ffi` surface, so changes there (see the root Cross-Package Sync table) usually need a matching edit here. The producer/consumer and origin shapes parallel `rs/moq-net`; keep names aligned with the Rust side.

## Conventions

- Document public symbols (the package ships `py.typed`; types and docstrings are the API). No em dashes in docstrings or comments.
- Async API: `Client`/`Server` are async context managers; iterate announcements/tracks with `async for`. Match the existing pattern in `client.py` examples.
- Tooling: `uv` workspace. Run via `just py <recipe>` (see `py/justfile`). Tests live under each package's `tests/`.
