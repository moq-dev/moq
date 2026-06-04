"""Smoke test: the raw uniffi bindings import and expose the core classes."""

import moq_ffi


def test_exports_core_classes():
    # A representative slice of the generated surface. If uniffi-bindgen output
    # or module-name wiring breaks, these attributes disappear.
    for name in ("MoqClient", "MoqServer", "MoqBroadcastProducer", "MoqError"):
        assert hasattr(moq_ffi, name), f"moq_ffi missing {name}"


def test_client_constructs():
    client = moq_ffi.MoqClient()
    assert client is not None
