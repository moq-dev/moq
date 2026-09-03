"""Server tests for end-to-end Server + Client over loopback with TLS."""

import asyncio
import struct

import moq
import moq_ffi
import pytest


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
    async with moq.Server("127.0.0.1:0", tls_generate=["localhost"]) as server:
        # Publish a broadcast on the server side.
        broadcast = server.create_broadcast("hello")
        media = broadcast.publish_audio(moq.AudioFormat.OPUS, opus_head())

        # Auto-accept incoming sessions in the background so the handshake
        # completes from the server side. Hold references so the sessions
        # outlive the test.
        sessions: list = []

        async def accept_loop() -> None:
            async for request in server:
                sessions.append(await request.accept())

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

                    broadcast_consumer = await client.request_broadcast(announcement.path)
                    catalog = await broadcast_consumer.catalog()
                    track_name, audio = next(iter(catalog.audio.items()))
                    assert audio.codec == "opus"

                    media_consumer = await broadcast_consumer.subscribe_media(track_name, audio)

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


async def test_client_reconnects_and_resumes_announcements():
    """#2609: the client rides out a transport drop on its own. A broadcast
    published only after the server kills the first session still reaches the
    client's announcements; the old one-shot session stalled silently forever."""
    async with moq.Server("127.0.0.1:0", tls_generate=["localhost"]) as server:
        sessions: list = []
        accepted = asyncio.Event()
        # Gate the redial. status() reports the current status rather than every
        # edge, so if the reconnect landed before we asked, CONNECTED -> CONNECTED
        # is coalesced away and the wait below would block until its timeout.
        regate = asyncio.Event()

        async def accept_loop() -> None:
            async for request in server:
                if sessions:
                    await regate.wait()
                sessions.append(await request.accept())
                accepted.set()

        accept_task = asyncio.create_task(accept_loop())

        try:
            async with moq.Client(
                f"https://{server.local_addr}",
                tls_verify=False,
                bind="127.0.0.1:0",
                # Fast retries so the test doesn't wait out the default 1s backoff.
                backoff=moq.Backoff(initial_ms=50, multiplier=2, max_ms=200, timeout_ms=0),
            ) as client:
                session = client.session
                assert session is not None
                assert await session.status() == moq.ConnectionStatus.CONNECTED

                # Kill the transport under the client, like a relay restart.
                await asyncio.wait_for(accepted.wait(), timeout=10)
                sessions[0].cancel(0)

                # Nothing accepts the redial until the gate opens, so DISCONNECTED
                # is still the current status when we ask for it.
                assert await asyncio.wait_for(session.status(), timeout=10) == moq.ConnectionStatus.DISCONNECTED

                # The client redials on its own; the accept loop serves it.
                regate.set()
                while await asyncio.wait_for(session.status(), timeout=10) != moq.ConnectionStatus.CONNECTED:
                    pass

                # A broadcast published only after the reconnect still arrives.
                broadcast = server.create_broadcast("after-reconnect")
                async for announcement in client.announced():
                    assert announcement.path == "after-reconnect"
                    break
                broadcast.finish()
        finally:
            accept_task.cancel()
            try:
                await accept_task
            except asyncio.CancelledError:
                pass


async def test_server_request_close():
    """A session reports when the server rejects its request."""
    async with moq.Server("127.0.0.1:0", tls_generate=["localhost"]) as server:

        async def reject_loop() -> None:
            async for request in server:
                await request.reject(403)

        reject_task = asyncio.create_task(reject_loop())
        try:
            client = moq_ffi.MoqClient()
            client.set_tls_disable_verify(True)
            client.set_bind("127.0.0.1:0")
            # One-shot, so this dial's outcome is what surfaces here rather than
            # whatever the reconnect loop eventually reports.
            client.set_reconnect(False)
            # The rejection races the optimistic connect: it surfaces either as a
            # connect error or as the session's terminal close. MoqError is an
            # Exception subclass at runtime; UniFFI's generated code rebinds the
            # name so the static checker doesn't see it.
            try:
                session = await asyncio.wait_for(client.connect(f"https://{server.local_addr}"), timeout=5.0)
            except moq_ffi.MoqError:  # type: ignore[misc]
                pass
            else:
                with pytest.raises(moq_ffi.MoqError):  # type: ignore[arg-type]
                    await asyncio.wait_for(session.closed(), timeout=5.0)
        finally:
            reject_task.cancel()
            try:
                await reject_task
            except asyncio.CancelledError:
                pass


async def test_cert_fingerprints_after_listen():
    """cert_fingerprints() returns hex SHA-256 once the server has bound."""
    async with moq.Server("127.0.0.1:0", tls_generate=["localhost"]) as server:
        fps = server.cert_fingerprints()
        assert len(fps) == 1
        assert len(fps[0]) == 64
        assert all(c in "0123456789abcdef" for c in fps[0])


async def test_request_double_accept_returns_already_responded():
    """Calling accept() twice on the same request raises AlreadyResponded."""
    async with moq.Server("127.0.0.1:0", tls_generate=["localhost"]) as server:
        sessions: list = []

        async def accept_once() -> None:
            async for request in server:
                sessions.append(await request.accept())
                # A second accept() must fail; MoqError is an Exception at runtime,
                # UniFFI's static rebind hides that from pyright.
                with pytest.raises(moq_ffi.MoqError):  # type: ignore[arg-type]
                    await request.accept()
                with pytest.raises(moq_ffi.MoqError):  # type: ignore[arg-type]
                    await request.reject(403)
                break

        accept_task = asyncio.create_task(accept_once())
        try:
            async with moq.Client(
                f"https://{server.local_addr}",
                tls_verify=False,
                bind="127.0.0.1:0",
            ):
                await asyncio.wait_for(accept_task, timeout=5.0)
        finally:
            if not accept_task.done():
                accept_task.cancel()
                try:
                    await accept_task
                except asyncio.CancelledError:
                    pass


async def test_serve_helper_accepts_clients():
    """Server.serve() accepts incoming sessions and holds them automatically."""
    async with moq.Server("127.0.0.1:0", tls_generate=["localhost"]) as server:
        broadcast = server.create_broadcast("via-serve")

        serve_task = asyncio.create_task(server.serve())
        try:
            async with moq.Client(
                f"https://{server.local_addr}",
                tls_verify=False,
                bind="127.0.0.1:0",
            ) as client:
                async for announcement in client.announced():
                    assert announcement.path == "via-serve"
                    break
        finally:
            serve_task.cancel()
            try:
                await serve_task
            except asyncio.CancelledError:
                pass
            broadcast.finish()


async def test_broadcast_route_over_wire():
    """A route received over the wire exposes its hop chain and cost."""
    async with moq.Server("127.0.0.1:0", tls_generate=["localhost"]) as server:
        broadcast = server.create_broadcast("with-route")

        serve_task = asyncio.create_task(server.serve())
        try:
            async with moq.Client(
                f"https://{server.local_addr}",
                tls_verify=False,
                bind="127.0.0.1:0",
            ) as client:
                async for announcement in client.announced():
                    assert announcement.path == "with-route"
                    assert announcement.active
                    route = announcement.route
                    assert all(isinstance(h, int) for h in route.hops)
                    # A route crossing at least one session carries a non-empty hop chain.
                    assert len(route.hops) >= 1
                    break
        finally:
            serve_task.cancel()
            try:
                await serve_task
            except asyncio.CancelledError:
                pass
            broadcast.finish()


async def test_route_update_observes_restart():
    """A route metadata update arrives as another active announcement.

    The publisher re-prices its announced route; the subscriber observes the new
    hop chain in place (no retraction), and cancelling retracts it.
    """
    origin = moq.OriginProducer()
    async with moq.Server("127.0.0.1:0", tls_generate=["localhost"], publish=origin) as server:
        announce = origin.announce("routed", moq.Route(hops=[42]))

        serve_task = asyncio.create_task(server.serve())
        try:
            async with moq.Client(
                f"https://{server.local_addr}",
                tls_verify=False,
                bind="127.0.0.1:0",
            ) as client:
                announced = client.announced()
                first = await asyncio.wait_for(announced.__anext__(), timeout=5.0)
                assert first.path == "routed"
                assert first.active
                assert 42 in first.route.hops
                assert 77 not in first.route.hops

                # The publisher advertises a longer chain: an in-place update.
                announce.update(moq.Route(hops=[42, 77]))
                updated = await asyncio.wait_for(announced.__anext__(), timeout=5.0)
                assert updated.path == "routed"
                assert updated.active
                assert 77 in updated.route.hops

                # Cancelling retracts the route.
                announce.cancel()
                ended = await asyncio.wait_for(announced.__anext__(), timeout=5.0)
                assert ended.path == "routed"
                assert not ended.active
        finally:
            serve_task.cancel()
            try:
                await serve_task
            except asyncio.CancelledError:
                pass
