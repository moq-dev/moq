"""Logging configuration for the underlying Rust layer."""

from __future__ import annotations

from ._uniffi import moq_log_level


def log_level(level: str = "info") -> None:
    """Initialize tracing/logging for the underlying Rust layer.

    `level` is one of "error", "warn", "info", "debug", "trace" (empty string
    defaults to "info"). Can only be called once per process; calling it again
    raises `moq.Error`.
    """
    moq_log_level(level)
