"""Ask QEMU to shut down the guest before systemd stops the VM process."""

import asyncio
import sys

from qemu.qmp import QMPClient


async def main():
    client = QMPClient("github-runner-vm")
    await client.connect(sys.argv[1])
    try:
        await client.execute("system_powerdown")
    finally:
        await client.disconnect()


asyncio.run(main())
