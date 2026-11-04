"""Socket lifetime and cancellation tests use explicit task barriers, without timers."""

import asyncio
import gc
import subprocess
import sys
from collections.abc import Coroutine
from typing import Any, TypeVar

import moq
import pytest
from moq import socket

T = TypeVar("T")


async def start(coro: Coroutine[Any, Any, T]) -> asyncio.Task[T]:
    """Return after the coroutine has reached its first suspension."""
    loop = asyncio.get_running_loop()
    polled = loop.create_future()

    async def run() -> T:
        loop.call_soon(polled.set_result, None)
        return await coro

    task = asyncio.create_task(run())
    await polled
    return task


def endpoints() -> tuple[socket.Endpoint, socket.Endpoint]:
    origin = moq.OriginProducer()
    return socket.Endpoint(origin, "room", "a"), socket.Endpoint(origin, "room", "b")


async def tcp(a: socket.Endpoint, b: socket.Endpoint) -> tuple[socket.TcpStream, socket.TcpStream]:
    incoming = await start(b.accept_tcp())
    dial = await start(a.dial_tcp(b.id))
    accepted = await (await incoming).accept()
    return await dial, accepted


async def test_tcp_halves_fin_and_peer_identity():
    a, b = endpoints()
    client, server = await tcp(a, b)
    assert (a.id, a.prefix, client.peer, server.peer) == ("a", "room", "b", "a")
    assert client.session == server.session
    assert len(client.session) == 32
    producer, reader, consumer = client.producer, client.consumer, server.consumer
    # Releasing the stream cannot release either half kept by the application.
    del client
    gc.collect()
    producer.write_frame(b"one")
    producer.write_frame(b"")
    producer.finish()
    del producer
    gc.collect()
    assert [frame.payload async for frame in consumer] == [b"one", b""]
    server.producer.finish()
    assert await reader.read_frame() is None
    a.close()
    b.close()


async def test_refusal_and_cancelled_wait_leave_endpoint_usable():
    a, b = endpoints()
    pending = await start(b.accept_tcp())
    pending.cancel()
    with pytest.raises(asyncio.CancelledError):
        await pending
    dial = await start(a.dial_tcp("b"))
    incoming = await b.accept_tcp()
    assert incoming.peer == "a"
    incoming.reject()
    with pytest.raises(moq.Error.NotFound):
        await dial
    with pytest.raises(moq.Error.Closed):
        incoming.reject()
    client, server = await tcp(a, b)
    assert client.session == server.session
    a.close()
    b.close()


async def test_cancelled_dial_releases_the_original_subscription():
    a, b = endpoints()
    incoming = await start(b.accept_tcp())
    dial = await start(a.dial_tcp("b"))
    offered = await incoming
    dial.cancel()
    with pytest.raises(asyncio.CancelledError):
        await dial
    with pytest.raises(moq.Error.Cancelled):
        await offered.accept()
    client, server = await tcp(a, b)
    assert client.session == server.session
    a.close()
    b.close()


async def test_tcp_rst_and_endpoint_close_are_errors():
    a, b = endpoints()
    client, server = await tcp(a, b)
    client.producer.abort(17)
    with pytest.raises(moq.Error.Protocol) as reset:
        await server.consumer.read_frame()
    assert reset.value.details.kind == moq.ProtocolKind.APP
    assert reset.value.details.code == 64 + 17
    client, server = await tcp(a, b)
    a.close()
    with pytest.raises(moq.Error.Cancelled):
        await server.consumer.read_frame()
    b.close()


async def test_track_default_retains_ascending_messages_and_subscription():
    a, b = endpoints()
    incoming = await start(b.accept_track())
    dial = await start(a.dial_track("b"))
    server = await (await incoming).accept()
    client = await dial
    assert (client.peer, server.peer, client.session) == ("b", "a", server.session)
    producer, reverse, consumer = client.producer, server.producer, server.consumer
    del server
    gc.collect()
    assert consumer.info().max_age is None
    for sequence, payload in ((1, b"later"), (0, b"first")):
        group = producer.create_group(sequence)
        group.write_frame(payload)
        group.finish()
    producer.finish()
    assert [(group.sequence, [frame.payload async for frame in group]) async for group in consumer] == [
        (0, [b"first"]),
        (1, [b"later"]),
    ]
    reverse.finish()
    assert await client.consumer.read_frame() is None
    a.close()
    b.close()


async def test_accept_queues_are_separate_and_unaccepted_protocol_is_unsupported():
    a, b = endpoints()
    tcp_wait = await start(b.accept_tcp())
    with pytest.raises(moq.Error.Unsupported):
        await a.dial_track("b")
    track_wait = await start(b.accept_track())
    tcp_dial = await start(a.dial_tcp("b"))
    track_dial = await start(a.dial_track("b"))
    tcp_offer, track_offer = await tcp_wait, await track_wait
    assert tcp_offer.session != track_offer.session
    tcp_server = await tcp_offer.accept()
    track_server = await track_offer.accept()
    tcp_client, track_client = await tcp_dial, await track_dial
    assert tcp_client.session == tcp_server.session
    assert track_client.session == track_server.session
    a.close()
    b.close()


def test_a_dial_waits_for_a_late_peer_without_a_deadline():
    subprocess.run([sys.executable, __file__, "clock"], check=True)


async def late_peer(clock):
    origin = moq.OriginProducer()
    async with socket.Endpoint(origin, "room", "a") as a:
        dial = await start(a.dial_tcp("b"))
        clock.advance(24 * 60 * 60)
        async with socket.Endpoint(origin, "room", "b") as b:
            incoming = await b.accept_tcp()
            accepted = await incoming.accept()
            connected = await dial
            assert connected.session == accepted.session
            connected.producer.finish()
            accepted.producer.finish()
            assert await connected.consumer.read_frame() is None
            assert await accepted.consumer.read_frame() is None


async def test_cancelling_acceptance_drops_the_original_dial_while_proving_routability():
    origin = moq.OriginProducer()
    endpoint = socket.Endpoint(origin, "room", "server")
    forged = origin.create_broadcast("room/.socket/forged")
    returning = forged.broadcast.requests()
    forged.announce()
    incoming = await start(endpoint.accept_tcp())
    broadcast = await origin.consume().request("room/.socket/server")
    original = await start(broadcast.subscribe_track("tcp/forged/server/0123456789abcdef0123456789abcdef"))
    offered = await incoming
    accepting = await start(offered.accept())
    # Proof that accept is parked on the return path, before cancelling it.
    returned = await returning.requested_track()
    assert returned.name.startswith("tcp/forged/server/")
    accepting.cancel()
    with pytest.raises(asyncio.CancelledError):
        await accepting
    with pytest.raises(moq.Error.Cancelled):
        await original
    endpoint.close()
    forged.broadcast.close()


async def test_dropping_the_endpoint_resets_a_live_session():
    a, b = endpoints()
    client, server = await tcp(a, b)
    del a
    gc.collect()
    with pytest.raises(moq.Error.Cancelled):
        await server.consumer.read_frame()
    with pytest.raises(moq.Error.Closed):
        await client.consumer.read_frame()
    b.close()


if __name__ == "__main__":
    from moq.testing import Clock

    asyncio.run(late_peer(Clock()))
