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
    async with moq.Client("https://cdn.moq.dev/anon") as client:
        async for event in client.announced():
            if isinstance(event, moq.AnnounceEventStart):
                broadcast = await client.request_broadcast(event.announce.prefix)
                print(await moq.media.catalog(broadcast))


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
   TrackDemand
   TrackDynamic
   TrackRequest
   GroupProducer
   GroupRequest
   AudioProducer
   VideoProducer
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
   AudioConsumer
```

## Media

`moq.media` owns catalogs, encoded-media importers, and container consumers.
Every importer takes a broadcast; single-track imports choose a `Named` or `Requested` target.

```{eval-rst}
.. currentmodule:: moq.media

.. autosummary::
   :toctree: api
   :nosignatures:

   CatalogProducer
   CatalogConsumer
   TrackProducer
   TrackStreamProducer
   ContainerProducer
   ContainerStreamProducer
   ContainerConsumer
   ContainerGroupConsumer
   Named
   Requested
   MediaFrame
   catalog
```

## JSON tracks

`moq.json` mirrors the `moq-json` crate: each type wraps a track from the
broadcast, and producers advertise it in the catalog.

```{eval-rst}
.. currentmodule:: moq.json

.. autosummary::
   :toctree: api
   :nosignatures:

   SnapshotProducer
   StreamProducer
   SnapshotConsumer
   StreamConsumer
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
   AnnounceConsumer
   AnnouncedBroadcast
   Announce
   AnnounceEvent
   AnnounceEventStart
   AnnounceEventUpdate
   AnnounceEventEnd
   AnnounceEventRestart
```

## Data types

These records, enums, and objects are re-exported from the native `moq_ffi` bindings; the
wrapper groups them under `moq` and `moq.media`, with owned duration records at the boundary. Their fields are defined on the
Rust side ([`moq-ffi`](https://crates.io/crates/moq-ffi)).

```{eval-rst}
.. currentmodule:: moq

.. autosummary::
   :toctree: api
   :nosignatures:

   media.Catalog
   media.Container
   Frame
   media.MediaFrame
   Datagram
   media.Video
   media.VideoHint
   media.VideoProperties
   VideoFrame
   VideoCodec
   VideoPixelFormat
   VideoEncoderInput
   VideoEncoderOutput
   VideoEncoderKind
   media.Dimensions
   media.Audio
   AudioFrame
   AudioCodec
   media.AudioFormat
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
   protocol_error
   log_level
   mint_epoch
   epoch_time
```

```{toctree}
:hidden:
:maxdepth: 2

self
```
