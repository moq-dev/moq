---
title: libmoq
description: C bindings for MoQ
---

# libmoq

[![docs.rs](https://docs.rs/libmoq/badge.svg)](https://docs.rs/libmoq)

C bindings for `moq-lite` via FFI, enabling MoQ integration in C/C++ applications and other languages.

## Overview

`libmoq` provides:

- **C API** - Header files for C integration
- **FFI bindings** - Safe Rust-to-C interface
- **Build system integration** - CMake and pkg-config support

## Installation

### From Source

```bash
git clone https://github.com/moq-dev/moq
cd moq/rs/libmoq
cargo build --release
```

The library will be in `target/release/libmoq.a` (static) or `target/release/libmoq.so` (dynamic).

### Using Cargo

```bash
cargo install libmoq
```

## Usage

See the [`rs/libmoq/README.md`](https://github.com/moq-dev/moq/blob/main/rs/libmoq/README.md) for the C API function signatures and the [`rs/libmoq/src/test.rs`](https://github.com/moq-dev/moq/blob/main/rs/libmoq/src/test.rs) for working usage examples.

## API Reference

Full API documentation: [docs.rs/libmoq](https://docs.rs/libmoq)

## Use Cases

- **C/C++ applications** - Native integration without Rust toolchain
- **Language bindings** - Build bindings for Python, Go, etc.
- **Legacy systems** - Integrate MoQ into existing C codebases
- **Embedded systems** - Where Rust runtime isn't available

## Next Steps

- Use [moq-lite](/rs/crate/moq-lite) for Rust applications
- Deploy a [relay server](/app/relay/)
- Read the [Concepts guide](/concept/)
