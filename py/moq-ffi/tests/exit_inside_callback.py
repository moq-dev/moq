"""Exit the interpreter while the runtime thread is still inside a continuation.

Run by test_exit.py as a subprocess, since the race is with interpreter
finalization itself. The Rust runtime thread wakes an awaiting coroutine
through the uniffi continuation callback, then needs the GIL back to return
from it. CPython (before 3.14) kills a foreign thread that asks for the GIL
once finalization has begun, by pthread_exit, whose unwind cannot pass through
Rust frames and aborts the process.

Under load the thread is descheduled between waking the event loop and
returning, and the main thread runs on to exit meanwhile. Here that order is
forced: the thread sleeps inside the callback, and the main thread keeps the
GIL from before it wakes until finalization, so it is queued for the GIL
exactly when the interpreter starts refusing it.
"""

import asyncio
import sys
import threading
import time

import moq_ffi
import moq_ffi._uniffi.moq as gen

# A thread waiting for the GIL would otherwise force the main thread off it
# within 5ms, which lets it back in before finalization.
sys.setswitchinterval(5)

MAIN = threading.get_ident()
_continuation = gen._uniffi_continuation_callback
_local = threading.local()


class _Hold:
    # Parked in the runtime thread's interpreter state, which finalization clears
    # first thing, so without a runtime stop at atexit this sleep is where the
    # queued thread is handed the GIL and killed, and it keeps the process alive
    # for that abort to set the exit code.
    def __del__(self, sleep=time.sleep):
        sleep(0.2)


@gen._UNIFFI_RUST_FUTURE_CONTINUATION_CALLBACK
def _preempted_continuation(future_ptr, poll_code):
    _continuation(future_ptr, poll_code)
    if threading.get_ident() != MAIN:
        _local.hold = _Hold()
        # Releases the GIL, so the main thread runs on to exit meanwhile.
        time.sleep(0.02)


gen._uniffi_continuation_callback = _preempted_continuation


async def main() -> None:
    origin = moq_ffi.MoqOriginProducer(moq_ffi.MoqOriginConfig())
    broadcast = origin.create_broadcast("exit")
    broadcast.announce(moq_ffi.MoqRoute())
    track = broadcast.publish_track("data", None)

    consumer = await origin.consume().request_broadcast("exit")
    subscriber = await consumer.subscribe_track("data", None)

    # Written once the read is parked, so the runtime thread delivers the wake.
    asyncio.get_running_loop().call_later(0.05, track.write_frame, moq_ffi.MoqFrame(payload=b"hello"))
    frame = await subscriber.read_frame()
    assert frame is not None
    # Flushed now: a write left for exit would hand the GIL over early.
    print("received", frame.payload.decode(), flush=True)


asyncio.run(main())

# Hold the GIL, with no syscall that would release it, until the runtime
# thread has woken and queued behind it.
deadline = time.monotonic() + 0.1
while time.monotonic() < deadline:
    pass
