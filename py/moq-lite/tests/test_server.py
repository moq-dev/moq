"""Server tests — end-to-end Server + Client over loopback with TLS."""

import asyncio
import struct


def opus_head() -> bytes:
    return (
        b"OpusHead"
        + bytes([1, 2])
        + struct.pack("<H", 0)
        + struct.pack("<I", 48000)
        + struct.pack("<H", 0)
        + bytes([0])
    )


async def test_server_client_roundtrip():
    """Server publishes a broadcast; a client connects and receives a frame."""
    import moq_lite as moq

    async with moq.Server("127.0.0.1:0", tls_generate=["localhost"]) as server:
        # Publish a broadcast on the server side.
        broadcast = moq.BroadcastProducer()
        media = broadcast.publish_media("opus", opus_head())
        server.publish("hello", broadcast)

        # Auto-accept incoming sessions in the background so the handshake
        # completes from the server side. Hold references so the sessions
        # outlive the test.
        sessions: list = []

        async def accept_loop() -> None:
            async for request in server:
                sessions.append(await request.ok())

        accept_task = asyncio.create_task(accept_loop())

        try:
            # Connect a client and consume the broadcast.
            async with moq.Client(
                f"https://{server.local_addr}",
                tls_verify=False,
                bind="127.0.0.1:0",
            ) as client:
                async for announcement in client.announced():
                    assert announcement.path == "hello"

                    catalog = await announcement.broadcast.catalog()
                    track_name, audio = next(iter(catalog.audio.items()))
                    assert audio.codec == "opus"

                    media_consumer = announcement.broadcast.subscribe_media(track_name, audio.container, 10_000)

                    payload = b"hello over the wire"
                    media.write_frame(payload, 1_000_000)

                    async for frame in media_consumer:
                        assert frame.payload == payload
                        assert frame.timestamp_us == 1_000_000
                        break

                    break
        finally:
            accept_task.cancel()
            try:
                await accept_task
            except asyncio.CancelledError:
                pass
            media.finish()
            broadcast.finish()


async def test_server_request_close():
    """A client connecting to a server that rejects requests sees a connect failure."""
    import moq_ffi
    import moq_lite as moq
    import pytest

    async with moq.Server("127.0.0.1:0", tls_generate=["localhost"]) as server:

        async def reject_loop() -> None:
            async for request in server:
                await request.close(403)

        reject_task = asyncio.create_task(reject_loop())
        try:
            client = moq_ffi.MoqClient()
            client.set_tls_disable_verify(True)
            client.set_bind("127.0.0.1:0")
            # MoqError is an Exception subclass at runtime; UniFFI's generated
            # code rebinds the name so the static checker doesn't see it.
            with pytest.raises(moq_ffi.MoqError):  # type: ignore[arg-type]
                await asyncio.wait_for(client.connect(f"https://{server.local_addr}"), timeout=5.0)
        finally:
            reject_task.cancel()
            try:
                await reject_task
            except asyncio.CancelledError:
                pass
