"""Test-only control of the native MoQ clock.

Needs a ``moq-ffi`` built with its ``testing`` feature, as ``just py test`` does;
released wheels leave it out.
"""

import ctypes
import math
from typing import Any

from moq_ffi._uniffi.moq import _UniffiLib


def _symbol(name: str, argtypes: tuple[Any, ...], restype: Any) -> Any:
    try:
        function = getattr(_UniffiLib, name)
    except AttributeError:
        raise RuntimeError("moq-ffi was built without its `testing` feature") from None
    function.argtypes = argtypes
    function.restype = restype
    return function


class Clock:
    """The native clock, moved only by :meth:`advance`.

    Native timers never fire on their own, even while every task is idle, so a
    test steps native time in lockstep with its own loop. Construct it before
    any other :mod:`moq` object: the native runtime is process-global and starts
    on the host clock otherwise, with no way back, so give it a dedicated process.
    """

    def __init__(self) -> None:
        self._advance = _symbol("moq_ffi_test_clock_advance", (ctypes.c_uint64,), ctypes.c_bool)
        self._now = _symbol("moq_ffi_test_clock_now", (), ctypes.c_uint64)
        if not _symbol("moq_ffi_test_clock_start", (), ctypes.c_bool)():
            raise RuntimeError("the native runtime already started on the host clock")

    def advance(self, seconds: float) -> None:
        """Let ``seconds`` pass, waking every native timer due by then."""
        if not math.isfinite(seconds) or seconds < 0:
            raise ValueError("clock advance must be finite and non-negative")
        nanos = round(seconds * 1_000_000_000)
        if nanos >= 2**64 or not self._advance(nanos):
            raise OverflowError("clock advance runs past the clock's range")

    def time(self) -> float:
        """Seconds elapsed since the clock started."""
        return self._now() / 1_000_000_000
