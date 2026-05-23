"""End-to-end smoke test for the moq-ffi Python bindings server API.

Spawns a `MoqServer` with a self-signed certificate, connects a `MoqClient`,
and exits 0 on a successful handshake.

Usage:

    # Build the cdylib and generate the Python bindings.
    cargo build --release --package moq-ffi
    cargo run --bin uniffi-bindgen -- generate \
        --library target/release/libmoq_ffi.so \
        --language python --out-dir target/py-bindings

    # Run the smoke test against the generated bindings.
    PYTHONPATH=target/py-bindings python rs/moq-ffi/examples/server_smoke.py

Expected output:
    server bound on 127.0.0.1:NNNNN
    client connected, ok
"""

import asyncio
import sys

import moq


async def main() -> int:
    server = moq.MoqServer()
    server.set_bind("127.0.0.1:0")
    server.set_tls_generate(["localhost"])

    addr = await server.listen()
    print(f"server bound on {addr}")

    async def accept_one() -> moq.MoqSession:
        request = await server.accept()
        assert request is not None, "server.accept() returned None"
        return await request.ok()

    accept_task = asyncio.create_task(accept_one())

    client = moq.MoqClient()
    client.set_tls_disable_verify(True)
    client.set_bind("127.0.0.1:0")

    client_session = await client.connect(f"https://{addr}")
    server_session = await accept_task

    print("client connected, ok")

    client_session.cancel(0)
    server_session.cancel(0)
    server.cancel()
    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
