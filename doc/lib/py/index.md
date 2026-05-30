---
title: Python Libraries
description: Python implementation for MoQ pub/sub
---

# Python Libraries

The Python bindings expose [Media over QUIC](/) to scripts, services, and prototype tooling. Built on the same Rust core ([moq-ffi](https://crates.io/crates/moq-ffi)) as the Swift and Kotlin packages, wrapped with an idiomatic asyncio API.

## Packages

Two packages, split so the ergonomic API can evolve on its own cadence:

### moq-rs

[![PyPI](https://img.shields.io/pypi/v/moq-rs)](https://pypi.org/project/moq-rs/)

The package you want. Install `moq-rs` (the `moq` name is taken on PyPI), import `moq`. Real-time pub/sub with built-in caching, fan-out, and prioritization on top of QUIC, with a Pythonic API (no `Moq` prefixes, async context managers, async iterators). At session setup it negotiates either the `moq-lite` or `moq-transport` wire protocol.

It is pure Python and depends on `moq-ffi` via a compatible-release pin, so it floats to the latest `moq-ffi` patch automatically. It is versioned independently of the Rust crates.

### moq-ffi

[![PyPI](https://img.shields.io/pypi/v/moq-ffi)](https://pypi.org/project/moq-ffi/)

The raw UniFFI bindings (the `Moq`-prefixed classes), tracking the [`moq-ffi`](https://crates.io/crates/moq-ffi) Rust crate one-to-one. `moq-rs` pulls this in for you. Install it directly only if you need the unwrapped API or are building your own wrapper.

## Installation

```bash
pip install moq-rs
```

This pulls in `moq-ffi`, for which prebuilt wheels are published for:

- Linux x86_64 / aarch64 (manylinux_2_28)
- macOS x86_64 / aarch64
- Windows x86_64

For other platforms (Alpine, BSD, etc.) `pip` falls back to building `moq-ffi` from source via the published sdist. You'll need a Rust toolchain and a C compiler.

## Quickstart

### Subscribe

```python
import asyncio
import moq

async def main():
    async with moq.Client("https://relay.quic.video") as client:
        async for announcement in client.announced():
            catalog = await announcement.broadcast.catalog()

            for name in catalog.audio:
                async for frame in announcement.broadcast.subscribe_media(name):
                    print(f"frame: {len(frame.payload)} bytes, ts={frame.timestamp_us}")

asyncio.run(main())
```

### Publish

```python
import asyncio
import moq

async def main():
    async with moq.Client("https://relay.quic.video") as client:
        broadcast = moq.BroadcastProducer()
        audio = broadcast.publish_media("opus", opus_init_bytes)
        client.publish("my-stream", broadcast)

        audio.write_frame(payload, timestamp_us=0)
        audio.write_frame(payload, timestamp_us=20_000)

        audio.finish()
        broadcast.finish()

asyncio.run(main())
```

## Source and issues

- Source: [py/moq-rs](https://github.com/moq-dev/moq/tree/main/py/moq-rs) (wrapper), [py/moq-ffi](https://github.com/moq-dev/moq/tree/main/py/moq-ffi) (raw bindings)
- README: [py/moq-rs/README.md](https://github.com/moq-dev/moq/blob/main/py/moq-rs/README.md)
- Example scripts: [py/moq-rs/examples](https://github.com/moq-dev/moq/tree/main/py/moq-rs/examples)
