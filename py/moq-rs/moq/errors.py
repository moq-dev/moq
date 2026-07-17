"""Classification helpers for the errors raised across the FFI boundary."""

from __future__ import annotations

from moq_ffi import MoqError


def is_shutdown(err: BaseException) -> bool:
    """True for `Cancelled` and `Closed`, which arise from graceful shutdown
    rather than actual failures.

    Useful for breaking out of an `async for` without treating the expected
    end-of-stream error as a problem.
    """
    return isinstance(err, (MoqError.Cancelled, MoqError.Closed))


def is_auth(err: BaseException) -> bool:
    """True for `Unauthorized` (HTTP 401) and `Forbidden` (HTTP 403), which the
    server returns to reject a connection on authentication or authorization
    grounds.

    Unlike a transport failure, retrying without new credentials won't help, so
    callers should surface these rather than reconnect.
    """
    return isinstance(err, (MoqError.Unauthorized, MoqError.Forbidden))
