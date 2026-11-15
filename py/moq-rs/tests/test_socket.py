"""Socket lifetime and cancellation tests use explicit task barriers, without timers."""

import asyncio
import gc
import subprocess
import sys
from collections.abc import Coroutine
from typing import Any, TypeVar, cast

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


async def connect(a: socket.Endpoint, b: socket.Endpoint) -> tuple[socket.TrackStream, socket.TrackStream]:
    incoming = await start(b.accept_track())
    dial = await start(a.dial_track(b.id))
    accepted = await (await incoming).accept()
    return await dial, accepted


async def test_refusal_and_cancelled_wait_leave_endpoint_usable():
    a, b = endpoints()
    pending = await start(b.accept_track())
    pending.cancel()
    with pytest.raises(asyncio.CancelledError):
        await pending
    dial = await start(a.dial_track("b"))
    incoming = await b.accept_track()
    assert incoming.peer == "a"
    incoming.reject()
    with pytest.raises(moq.Error.NotFound):
        await dial
    with pytest.raises(moq.Error.AlreadyResponded):
        incoming.reject()
    client, server = await connect(a, b)
    assert client.session == server.session
    a.close()
    b.close()


async def test_cancelled_dial_releases_the_original_subscription():
    a, b = endpoints()
    incoming = await start(b.accept_track())
    dial = await start(a.dial_track("b"))
    offered = await incoming
    dial.cancel()
    with pytest.raises(asyncio.CancelledError):
        await dial
    with pytest.raises(moq.Error.Cancelled):
        await offered.accept()
    client, server = await connect(a, b)
    assert client.session == server.session
    a.close()
    b.close()


async def test_abort_and_endpoint_close_are_errors():
    a, b = endpoints()
    client, server = await connect(a, b)
    client.producer.abort(17)
    with pytest.raises(moq.Error.Protocol) as reset:
        await server.consumer.read_frame()
    assert reset.value.details.kind == moq.ProtocolKind.APP
    assert reset.value.details.code == 64 + 17
    client, server = await connect(a, b)
    a.close()
    with pytest.raises(moq.Error.Closed):
        await server.consumer.read_frame()
    b.close()


async def test_track_default_retains_ascending_messages_and_subscription():
    a, b = endpoints()
    client, server = await connect(a, b)
    assert (a.id, a.prefix, client.peer, server.peer) == ("a", "room", "b", "a")
    assert client.session == server.session
    assert len(client.session) == 32
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


async def test_a_dial_in_another_protocol_is_unsupported():
    origin = moq.OriginProducer()
    endpoint = socket.Endpoint(origin, "room", "server")
    waiting = await start(endpoint.accept_track())
    broadcast = await origin.consume().request("room/.server/server")
    with pytest.raises(moq.Error.Unsupported):
        await broadcast.subscribe_track("tcp/client/server/0123456789abcdef0123456789abcdef")
    assert not waiting.done()
    endpoint.close()
    with pytest.raises(moq.Error.Closed):
        await waiting


def test_a_dial_waits_for_a_late_peer_without_a_deadline():
    subprocess.run([sys.executable, __file__, "clock"], check=True)


async def late_peer(clock):
    origin = moq.OriginProducer()
    async with socket.Endpoint(origin, "room", "a") as a:
        dial = await start(a.dial_track("b"))
        clock.advance(24 * 60 * 60)
        async with socket.Endpoint(origin, "room", "b") as b:
            incoming = await b.accept_track()
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
    forged = origin.route("room/.client/forged", hidden=True)
    returning = forged.broadcast.requests()
    forged.announce()
    incoming = await start(endpoint.accept_track())
    broadcast = await origin.consume().request("room/.server/server")
    original = await start(broadcast.subscribe_track("track/forged/server/0123456789abcdef0123456789abcdef"))
    offered = await incoming
    accepting = await start(offered.accept())
    # Proof that accept is parked on the return path, before cancelling it.
    returned = await returning.requested_track()
    assert returned.name.startswith("track/forged/server/")
    accepting.cancel()
    with pytest.raises(asyncio.CancelledError):
        await accepting
    with pytest.raises(moq.Error.Cancelled):
        await original
    endpoint.close()
    forged.broadcast.close()


async def test_dropping_the_endpoint_resets_a_live_session():
    a, b = endpoints()
    client, server = await connect(a, b)
    del a
    gc.collect()
    # Withdrawal and the session's cancellation can reach the reader first; the
    # Rust contract guarantees a reset, with no clean EOF or promised cause.
    reset = cast(type[Exception], moq.Error)
    with pytest.raises(reset):
        await server.consumer.read_frame()
    # The peer's own half stays open until it ends it, here by closing.
    b.close()
    with pytest.raises(reset):
        await client.consumer.read_frame()


if __name__ == "__main__":
    from clock import Clock

    asyncio.run(late_peer(Clock()))
