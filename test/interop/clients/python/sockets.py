"""Python/Rust socket interop through a relay, in both directions and protocols."""

import argparse
import asyncio
import contextlib
from collections.abc import Coroutine
from typing import Any, TypeVar

import moq
from moq import socket

PAYLOADS = [b"first", b"", b"last"]
T = TypeVar("T")


async def start(coro: Coroutine[Any, Any, T]) -> asyncio.Task[T]:
    """Enable an accept queue before another process can dial it."""
    loop = asyncio.get_running_loop()
    polled = loop.create_future()

    async def run() -> T:
        loop.call_soon(polled.set_result, None)
        return await coro

    task = asyncio.create_task(run())
    await polled
    return task


async def exchange(stream: socket.TcpStream | socket.TrackStream) -> None:
    assert stream.peer == "rust"
    if isinstance(stream, socket.TcpStream):
        for payload in PAYLOADS:
            stream.producer.write_frame(payload)
        stream.producer.finish()
        assert [frame.payload async for frame in stream.consumer] == PAYLOADS
    else:
        assert stream.consumer.info().max_age is None
        for _ in range(3):
            group = stream.producer.append_group()
            for payload in PAYLOADS:
                group.write_frame(payload)
            group.finish()
        stream.producer.finish()
        assert [(group.sequence, [frame.payload async for frame in group]) async for group in stream.consumer] == [
            (sequence, PAYLOADS) for sequence in range(3)
        ]


async def main(args: argparse.Namespace) -> None:
    origin = moq.OriginProducer()
    async with moq.Client(
        args.url, publish=origin, consume=origin, versions=[args.version], tls=moq.ClientTls(insecure=True)
    ):
        for protocol in ("tcp", "track"):
            for role in ("dial", "accept"):
                prefix = f"{args.prefix}/{protocol}/{role}"
                async with socket.Endpoint(origin, prefix, "python") as endpoint:
                    incoming = None
                    if role == "dial":
                        incoming = await start(endpoint.accept_tcp() if protocol == "tcp" else endpoint.accept_track())
                    peer = await asyncio.create_subprocess_exec(
                        args.rust,
                        args.url,
                        prefix,
                        args.version,
                        protocol,
                        role,
                        stdin=asyncio.subprocess.PIPE,
                        stdout=asyncio.subprocess.PIPE,
                    )
                    assert peer.stdout is not None and peer.stdin is not None
                    try:
                        if incoming is not None:
                            stream = await (await incoming).accept()
                        else:
                            assert await peer.stdout.readline() == b"listening\n"
                            stream = await (
                                endpoint.dial_tcp("rust") if protocol == "tcp" else endpoint.dial_track("rust")
                            )
                        await exchange(stream)
                        assert await peer.stdout.readline() == b"drained\n"
                        peer.stdin.write(b"drained\n")
                        await peer.stdin.drain()
                        assert await peer.wait() == 0
                        print(f"socket {protocol}, Rust {role}: passed", flush=True)
                    finally:
                        if incoming is not None and not incoming.done():
                            incoming.cancel()
                            with contextlib.suppress(asyncio.CancelledError):
                                await incoming
                        if peer.returncode is None:
                            peer.kill()
                        await peer.wait()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("url")
    parser.add_argument("prefix")
    parser.add_argument("version")
    parser.add_argument("rust")
    asyncio.run(main(parser.parse_args()))
