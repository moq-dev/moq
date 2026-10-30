"""Server tests for end-to-end Server + Client over loopback with TLS."""

import asyncio
import struct
from datetime import timedelta

import moq
import moq_ffi
import pytest


async def routes(announced: moq.AnnounceConsumer):
    """Yield each newly announced route, skipping updates and ends."""
    async for event in announced:
        if isinstance(event, moq.AnnounceEventStart):
            yield event.announce


async def next_route(announced: moq.AnnounceConsumer) -> moq.AnnounceEvent:
    """The next announce event."""
    return await asyncio.wait_for(anext(announced), timeout=5.0)


def opus_head() -> bytes:
    return (
        b"OpusHead"
        + bytes([1, 2])
        + struct.pack("<H", 0)
        + struct.pack("<I", 48000)
        + struct.pack("<H", 0)
        + bytes([0])
    )


async def wait_phase(session: moq.Session, phase: moq.ConnectionPhase) -> moq.ConnectionState:
    state = session.state()
    while state.phase != phase:
        state = await asyncio.wait_for(session.state_changed(state.revision), timeout=10)
    return state


async def test_server_client_roundtrip():
    """Server publishes a broadcast; a client connects and receives a frame."""
    async with moq.Server("127.0.0.1:0", versions=["moq-lite-07-wip"], tls_generate=["localhost"]) as server:
        # Publish a broadcast on the server side.
        publication = server.create_broadcast("hello")
        broadcast = publication.broadcast
        media = moq.media.TrackProducer.audio(
            broadcast, moq.media.AudioInit(format=moq.media.AudioFormat.OPUS, data=opus_head())
        )
        publication.announce()

        # Auto-accept incoming sessions in the background so the handshake
        # completes from the server side. Hold references so the sessions
        # outlive the test.
        sessions: list = []
        implementations: list = []

        async def accept_loop() -> None:
            async for request in server:
                implementations.append(request.peer_implementation)
                sessions.append(await request.accept())

        accept_task = asyncio.create_task(accept_loop())

        try:
            # Connect a client and consume the broadcast.
            async with moq.Client(
                f"https://{server.local_addr}",
                tls=moq.ClientTls(insecure=True),
                bind="127.0.0.1:0",
                versions=["moq-lite-07-wip"],
            ) as client:
                async for announcement in routes(client.announced()):
                    assert announcement.prefix == "hello"

                    broadcast_consumer = await client.request(announcement.prefix)
                    catalog = await moq.media.catalog(broadcast_consumer)
                    track_name, audio = next(iter(catalog.audio.items()))
                    assert audio.codec == "opus"

                    media_consumer = await moq.media.TrackConsumer.subscribe(broadcast_consumer, track_name, audio)

                    payload = b"hello over the wire"
                    media.write_frame(payload, timedelta(microseconds=1_000_000))

                    async for frame in media_consumer:
                        assert isinstance(frame, moq.media.MediaFrame)
                        assert frame.payload == payload
                        assert frame.timestamp // timedelta(microseconds=1) == 1_000_000
                        break

                    break

            # The wrapper names itself ahead of moq-ffi, so a server can tell bindings apart.
            assert implementations[0].startswith("moq-py/")
        finally:
            accept_task.cancel()
            try:
                await accept_task
            except asyncio.CancelledError:
                pass
            media.finish()
            broadcast.close()


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
                tls=moq.ClientTls(insecure=True),
                bind="127.0.0.1:0",
                # Fast retries so the test doesn't wait out the default 1s backoff.
                backoff=moq.Backoff(
                    initial=timedelta(milliseconds=50),
                    multiplier=2,
                    max=timedelta(milliseconds=200),
                    timeout=timedelta(0),
                ),
            ) as client:
                session = client.session
                assert session is not None
                state = await wait_phase(session, moq.ConnectionPhase.CONNECTED)
                assert state.connects == 1
                assert session.state().failure is None
                assert session.stats().version is not None

                # Kill the transport under the client, like a relay restart.
                await asyncio.wait_for(accepted.wait(), timeout=10)
                sessions[0].cancel(0)

                # Nothing accepts the redial until the gate opens, so DISCONNECTED
                # is still the current status when we ask for it.
                state = await wait_phase(session, moq.ConnectionPhase.DISCONNECTED)
                # The drop's reason is recorded before the status moves.
                assert session.state().failure is not None
                assert session.stats().version is None

                # The client redials on its own; the accept loop serves it.
                regate.set()
                state = await wait_phase(session, moq.ConnectionPhase.CONNECTED)
                assert state.connects == 2
                assert session.state().failure is None

                # A broadcast published only after the reconnect still arrives.
                publication = server.create_broadcast("after-reconnect")
                broadcast = publication.broadcast
                publication.announce()
                async for announcement in routes(client.announced()):
                    assert announcement.prefix == "after-reconnect"
                    break
                broadcast.close()
        finally:
            accept_task.cancel()
            try:
                await accept_task
            except asyncio.CancelledError:
                pass


async def test_session_state_names_a_failing_first_dial():
    """A first dial returns a session whose state reports each failed attempt."""
    import socket

    # A socket that never answers holds the port, so each dial times out.
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as silent:
        silent.bind(("127.0.0.1", 0))
        host, port = silent.getsockname()
        client = moq.Client(
            f"https://{host}:{port}",
            tls=moq.ClientTls(insecure=True),
            bind="127.0.0.1:0",
            timeout=timedelta(milliseconds=100),
            websocket=moq.WebSocketConfig(enabled=False),
            backoff=moq.Backoff(
                initial=timedelta(milliseconds=50),
                multiplier=2,
                max=timedelta(milliseconds=200),
                timeout=timedelta(0),
            ),
        )
        async with client:
            session = client.session
            assert session is not None
            state = session.state()
            assert state.phase == moq.ConnectionPhase.CONNECTING
            assert state.connects == 0
            for _ in range(2):
                state = await asyncio.wait_for(session.state_changed(state.revision), timeout=10)
                assert isinstance(state.failure, moq.Error.Connect)
                assert state.connects == 0
            session.cancel(0)


async def test_server_close_releases_port():
    """Exiting the context manager releases the listening socket before it
    returns, so the same address binds again with no retry."""
    async with moq.Server("127.0.0.1:0", tls_generate=["localhost"]) as server:
        addr = server.local_addr

    # No retry: __aexit__ closed the socket, so this binds on the first try.
    async with moq.Server(addr, tls_generate=["localhost"]) as rebound:
        assert rebound.local_addr == addr


async def test_server_request_close():
    """A session reports when the server rejects its request."""
    async with moq.Server("127.0.0.1:0", tls_generate=["localhost"]) as server:

        async def reject_loop() -> None:
            async for request in server:
                await request.reject(403)

        reject_task = asyncio.create_task(reject_loop())
        try:
            client = moq_ffi.MoqClient(
                moq_ffi.MoqClientConfig(
                    bind="127.0.0.1:0",
                    tls=moq_ffi.MoqClientTls(insecure=True),
                    # One-shot, so this dial's outcome is what surfaces here rather than
                    # whatever the reconnect loop eventually reports.
                    once=True,
                )
            )
            # Network rejection belongs to the immediately returned session.
            session = await asyncio.wait_for(client.connect(f"https://{server.local_addr}"), timeout=5.0)
            with pytest.raises(moq_ffi.MoqError):  # type: ignore[arg-type]
                await asyncio.wait_for(session.closed(), timeout=5.0)
            state = session.state()
            assert state.phase == moq_ffi.MoqConnectionPhase.CLOSED
            assert state.failure is not None
        finally:
            reject_task.cancel()
            try:
                await reject_task
            except asyncio.CancelledError:
                pass


async def test_client_websocket_fallback_options():
    """The fallback knobs reach the native client, and QUIC alone still connects."""
    async with moq.Server("127.0.0.1:0", tls_generate=["localhost"]) as server:
        sessions: list = []

        async def accept_loop() -> None:
            async for request in server:
                sessions.append(await request.accept())

        accept_task = asyncio.create_task(accept_loop())
        try:
            async with moq.Client(
                f"https://{server.local_addr}",
                tls=moq.ClientTls(insecure=True),
                bind="127.0.0.1:0",
                websocket=moq.WebSocketConfig(enabled=False, delay=timedelta(0)),
            ) as client:
                assert client.session is not None
        finally:
            accept_task.cancel()
            try:
                await accept_task
            except asyncio.CancelledError:
                pass

    # A negative duration fails on enter, where the native client is built.
    with pytest.raises(ValueError):
        async with moq.Client("https://localhost", websocket=moq.WebSocketConfig(delay=timedelta(milliseconds=-1))):
            pass


async def test_client_connection_knobs():
    """Every connection knob reaches the native client, and the dial still connects."""
    assert moq.versions()
    async with moq.Server("127.0.0.1:0", tls_generate=["localhost"]) as server:
        sessions: list = []

        async def accept_loop() -> None:
            async for request in server:
                sessions.append(await request.accept())

        accept_task = asyncio.create_task(accept_loop())
        try:
            async with moq.Client(
                f"https://{server.local_addr}",
                tls=moq.ClientTls(fingerprints=server.cert_fingerprints(), host_name="localhost"),
                bind="127.0.0.1:0",
                versions=moq.versions(),
                timeout=timedelta(seconds=5),
                race=timedelta(0),
                resolution_delay=timedelta(0),
                quic=moq.QuicConfig(
                    idle_timeout=timedelta(seconds=5),
                    keep_alive=timedelta(0),
                    gso=False,
                    mtu_discovery=True,
                    congestion_control=moq.CongestionControl.LOSS,
                    keylog=False,
                ),
                websocket=moq.WebSocketConfig(enabled=False),
            ) as client:
                assert client.session is not None
        finally:
            accept_task.cancel()
            try:
                await accept_task
            except asyncio.CancelledError:
                pass

    # A negative duration fails on enter, where the native client is built.
    with pytest.raises(ValueError):
        async with moq.Client("https://localhost", quic=moq.QuicConfig(idle_timeout=timedelta(milliseconds=-1))):
            pass


async def test_invalid_config_fails_on_enter():
    """A value the native side cannot use fails the connect or listen, not a later call."""
    with pytest.raises(moq.Error.Config):
        async with moq.Client("https://localhost", versions=["moq-lite-99"]):
            pass
    with pytest.raises(moq.Error.Config):
        async with moq.Server("not-an-address", tls_generate=["localhost"]):
            pass


async def test_server_keylog_requires_sslkeylogfile(monkeypatch):
    """keylog reaches the native server, which refuses it without SSLKEYLOGFILE."""
    monkeypatch.delenv("SSLKEYLOGFILE", raising=False)
    async with moq.Server("127.0.0.1:0", tls_generate=["localhost"], keylog=False):
        pass
    with pytest.raises(moq.Error.Config, match="SSLKEYLOGFILE"):
        async with moq.Server("127.0.0.1:0", tls_generate=["localhost"], keylog=True):
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
                tls=moq.ClientTls(insecure=True),
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
        publication = server.create_broadcast("via-serve")
        broadcast = publication.broadcast
        publication.announce()

        serve_task = asyncio.create_task(server.serve())
        try:
            async with moq.Client(
                f"https://{server.local_addr}",
                tls=moq.ClientTls(insecure=True),
                bind="127.0.0.1:0",
            ) as client:
                async for announcement in routes(client.announced()):
                    assert announcement.prefix == "via-serve"
                    break
        finally:
            serve_task.cancel()
            try:
                await serve_task
            except asyncio.CancelledError:
                pass
            broadcast.close()


async def test_broadcast_route_over_wire():
    """A route received over the wire exposes its hop chain and cost."""
    async with moq.Server("127.0.0.1:0", tls_generate=["localhost"]) as server:
        publication = server.create_broadcast("with-route")
        broadcast = publication.broadcast
        publication.announce()

        serve_task = asyncio.create_task(server.serve())
        try:
            async with moq.Client(
                f"https://{server.local_addr}",
                tls=moq.ClientTls(insecure=True),
                bind="127.0.0.1:0",
            ) as client:
                async for announcement in routes(client.announced()):
                    assert announcement.prefix == "with-route"
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
            broadcast.close()


async def test_route_update_observes_restart():
    """A route metadata update arrives as an AnnounceEventUpdate.

    The publisher re-prices its announced route; the subscriber observes the new
    cost in place (no retraction), and unannouncing retracts it.
    """
    origin = moq.OriginProducer()
    async with moq.Server("127.0.0.1:0", tls_generate=["localhost"], publish=origin) as server:
        publication = origin.create_broadcast("routed")
        publication.announce(moq.AnnounceRoute(cost=1))

        serve_task = asyncio.create_task(server.serve())
        try:
            async with moq.Client(
                f"https://{server.local_addr}",
                tls=moq.ClientTls(insecure=True),
                bind="127.0.0.1:0",
            ) as client:
                announced = client.announced()
                first = await next_route(announced)
                assert isinstance(first, moq.AnnounceEventStart)
                assert first.announce.prefix == "routed"
                first_cost = first.announce.route.cost

                # The publisher raises its cost: an in-place update.
                publication.announce(moq.AnnounceRoute(cost=5))
                updated = await next_route(announced)
                assert isinstance(updated, moq.AnnounceEventUpdate)
                assert updated.announce.prefix == "routed"
                assert updated.announce.route.cost == first_cost + 4

                # Unannouncing retracts the route.
                publication.unannounce()
                ended = await next_route(announced)
                assert isinstance(ended, moq.AnnounceEventEnd)
                assert ended.announce.prefix == "routed"
        finally:
            serve_task.cancel()
            try:
                await serve_task
            except asyncio.CancelledError:
                pass


async def test_client_context_keeps_the_body_error():
    """An error in the body survives the context manager, and the session is still
    cancelled. A live subscribed track never drains, so draining on the way out would
    wait out the deadline and replace the body's error with a delivery timeout."""
    async with moq.Server("127.0.0.1:0", tls_generate=["localhost"]) as server:
        sessions: list = []
        reading: list = []
        accepted = asyncio.Event()

        async def accept_loop() -> None:
            async for request in server:
                sessions.append(await request.accept())
                accepted.set()

        async def drain(reader: moq.TrackConsumer) -> None:
            async for _ in reader:
                pass

        accept_task = asyncio.create_task(accept_loop())
        broadcast = None
        session = None
        try:
            with pytest.raises(ZeroDivisionError):
                async with moq.Client(
                    f"https://{server.local_addr}",
                    tls=moq.ClientTls(insecure=True),
                    bind="127.0.0.1:0",
                ) as client:
                    # Held past the context manager, so the exit has to cancel it
                    # rather than lean on the last session reference dropping.
                    session = client.session
                    assert session is not None
                    publication = client.create_broadcast("live")
                    broadcast = publication.broadcast
                    publication.announce()
                    track = broadcast.publish_track("data")
                    await asyncio.wait_for(accepted.wait(), timeout=5.0)
                    consume = sessions[0].consume()
                    async for announcement in routes(consume.announced()):
                        assert announcement.prefix == "live"
                        break
                    consumer = await asyncio.wait_for(consume.request("live"), timeout=5.0)
                    reader = await asyncio.wait_for(consumer.subscribe_track("data"), timeout=5.0)
                    reading.append(asyncio.create_task(drain(reader)))
                    demand = track.demand()
                    while await asyncio.wait_for(demand.wanted_changed(), timeout=5.0) is None:
                        pass
                    raise ZeroDivisionError("boom")
            # The error exits the context without draining, but the session is over.
            assert session is not None
            await asyncio.wait_for(session.closed(), timeout=5.0)
        finally:
            accept_task.cancel()
            try:
                await accept_task
            except asyncio.CancelledError:
                pass
            if session is not None:
                # Tear down before the readers so this is deterministic whether or not
                # the exit above cancelled, and a failure surfaces as a failure rather
                # than a wedged event loop.
                session.cancel(0)
            for task in reading:
                task.cancel()
            await asyncio.gather(*reading, return_exceptions=True)
            if broadcast is not None:
                broadcast.close()


async def _await_grant(session: moq.Session, want: moq.Grant) -> None:
    async def follow() -> None:
        async with session.grants() as grants:
            async for grant in grants:
                if grant == want:
                    return

    await asyncio.wait_for(follow(), timeout=5.0)


async def test_session_auth_reads_the_grant():
    """Both sides read the grant their peer gave them; a peer that verifies no tokens refuses one."""
    version = ["moq-lite-07-wip"]
    async with moq.Server("127.0.0.1:0", tls_generate=["localhost"], versions=version) as server:
        sessions: list = []
        accepted = asyncio.Event()

        async def accept_loop() -> None:
            async for request in server:
                sessions.append(await request.accept())
                accepted.set()

        accept_task = asyncio.create_task(accept_loop())
        try:
            async with moq.Client(
                f"https://{server.local_addr}", tls=moq.ClientTls(insecure=True), bind="127.0.0.1:0", versions=version
            ) as client:
                session = client.session
                assert session is not None
                everything = moq.Grant(publish=("**",), subscribe=("**",))
                await _await_grant(session, everything)
                await asyncio.wait_for(accepted.wait(), timeout=5.0)
                await _await_grant(sessions[0], everything)
                assert sessions[0].tokens() is None

                tokens = session.tokens()
                assert tokens is not None
                tokens.set(["extra"])
                with pytest.raises(moq.Error.Unsupported):
                    await asyncio.wait_for(tokens.answered(), timeout=5.0)
                assert session.grants().grant() == everything
        finally:
            accept_task.cancel()
            try:
                await accept_task
            except asyncio.CancelledError:
                pass
            for session in sessions:
                session.cancel(0)
