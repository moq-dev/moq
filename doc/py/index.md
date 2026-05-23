---
title: Python Libraries
description: Python implementation for MoQ pub/sub
---

# Python Libraries

The Python bindings expose [Media over QUIC](/) to scripts, services, and prototype tooling. Built on the same Rust core ([moq-ffi](https://crates.io/crates/moq-ffi)) as the Swift and Kotlin packages, wrapped with an idiomatic asyncio API.

## Packages

### moq-net

[![PyPI](https://img.shields.io/pypi/v/moq-net)](https://pypi.org/project/moq-net/)

The networking layer for MoQ in Python: real-time pub/sub with built-in caching, fan-out, and prioritization on top of QUIC. At session setup it negotiates either the `moq-lite` or `moq-transport` wire protocol.

**Features:**

- Async context managers and async iterators throughout
- Native QUIC via the Rust [`moq-net`](/rs/crate/moq-net) crate
- WebCodecs-style catalog + container format via [`hang`](/rs/crate/hang)
- Pythonic API with no `Moq` prefixes ([more details](/py/moq-net))

[Learn more](/py/moq-net)

## Installation

```bash
pip install moq-net
```

Prebuilt wheels are published for:

- Linux x86_64 / aarch64 (manylinux_2_28)
- macOS x86_64 / aarch64
- Windows x86_64

For other platforms (Alpine, BSD, etc.) `pip` falls back to building from source via the published sdist. You'll need a Rust toolchain and a C compiler.

## Quickstart

### Subscribe

```python
import asyncio
import moq_net as moq

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
import moq_net as moq

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

- Source: [py/moq-net](https://github.com/moq-dev/moq/tree/main/py/moq-net)
- README: [py/moq-net/README.md](https://github.com/moq-dev/moq/blob/main/py/moq-net/README.md)
- Example scripts: [py/moq-net/examples](https://github.com/moq-dev/moq/tree/main/py/moq-net/examples)
