# moq

Python bindings for [Media over QUIC](https://moq.dev): real-time pub/sub with
built-in caching, fan-out, and prioritization over QUIC. This is the API
reference for the ergonomic `moq` wrapper (installed as
[`moq-rs`](https://pypi.org/project/moq-rs/)).

```bash
pip install moq-rs
```

```python
import asyncio
import moq


async def main():
    async with moq.Client("https://relay.quic.video") as client:
        async for announcement in client.announced():
            catalog = await announcement.broadcast.catalog()
            print(catalog)


asyncio.run(main())
```

## Connecting

```{eval-rst}
.. currentmodule:: moq

.. autosummary::
   :toctree: api
   :nosignatures:

   Client
   connect
   Server
   Session
   Request
   Transport
```

## Publishing

```{eval-rst}
.. currentmodule:: moq

.. autosummary::
   :toctree: api
   :nosignatures:

   BroadcastProducer
   BroadcastDynamic
   BroadcastRequest
   TrackProducer
   TrackDynamic
   TrackRequest
   GroupProducer
   GroupRequest
   MediaProducer
   MediaStreamProducer
   AudioProducer
   JsonSnapshotProducer
   JsonStreamProducer
```

## Subscribing

```{eval-rst}
.. currentmodule:: moq

.. autosummary::
   :toctree: api
   :nosignatures:

   BroadcastConsumer
   TrackConsumer
   GroupConsumer
   MediaConsumer
   AudioConsumer
   CatalogConsumer
   JsonSnapshotConsumer
   JsonStreamConsumer
```

## Origin and announcements

```{eval-rst}
.. currentmodule:: moq

.. autosummary::
   :toctree: api
   :nosignatures:

   OriginProducer
   OriginConsumer
   OriginDynamic
   Announced
   AnnouncedBroadcast
   Announcement
```

## Data types

These records and enums are re-exported from the native `moq_ffi` bindings; the
wrapper surfaces them under `moq` unchanged. Their fields are defined on the
Rust side ([`moq-ffi`](https://crates.io/crates/moq-ffi)).

```{eval-rst}
.. currentmodule:: moq

.. autosummary::
   :toctree: api
   :nosignatures:

   Catalog
   Container
   Frame
   MediaFrame
   Datagram
   Video
   VideoHint
   Dimensions
   Audio
   AudioFrame
   AudioCodec
   AudioFormat
   AudioDecoderOutput
   AudioEncoderInput
   AudioEncoderOutput
   Subscription
   TrackInfo
   FetchGroupOptions
   Route
   ConnectionStats
```

## Helpers

```{eval-rst}
.. currentmodule:: moq

.. autosummary::
   :toctree: api
   :nosignatures:

   Error
   is_auth
   is_shutdown
   log_level
```

```{toctree}
:hidden:
:maxdepth: 2

self
```
