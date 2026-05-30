# moq-ffi

Raw [UniFFI](https://mozilla.github.io/uniffi-rs/) bindings for the [Media over QUIC](https://github.com/moq-dev/moq) Rust crates.

This package is the native foundation: the compiled `moq-ffi` cdylib plus the auto-generated Python bindings, exposed exactly as uniffi-bindgen emits them (the `Moq`-prefixed classes). It tracks the [`moq-ffi`](https://crates.io/crates/moq-ffi) Rust crate version one-to-one.

**Most callers want [`moq`](https://pypi.org/project/moq/) instead**, the ergonomic wrapper with a Pythonic API (no `Moq` prefixes, async iterators, context managers). Use `moq-ffi` directly only if you need the unwrapped API or are building your own wrapper.

## Installation

```bash
pip install moq-ffi
```

The distribution is `moq-ffi`; the import name is `moq_ffi`.

```python
import moq_ffi

client = moq_ffi.MoqClient()
session = await client.connect("https://relay.quic.video")
```

## See Also

- [`moq`](https://pypi.org/project/moq/). The ergonomic wrapper most callers want.
- [`moq-ffi`](https://crates.io/crates/moq-ffi). The Rust crate that produces these bindings.
- [MoQ project](https://github.com/moq-dev/moq). Full monorepo with Rust server, TypeScript browser lib, and more.
