"""Loaded by the runner's Python: fixes a CPython race that fails its pcap checks.

`Popen.send_signal()` polls before signalling, so when pyshark kills a tshark
that already exited, the poll reaps it before asyncio's child watcher does. The
watcher then finds no child and reports exit status 255, which the transport
records over the real status the poll stored, and pyshark raises
TSharkCrashException for a tshark that exited cleanly. Prefer the polled status.
"""

import asyncio.base_subprocess

_process_exited = asyncio.base_subprocess.BaseSubprocessTransport._process_exited


def _keep_polled_status(self, returncode):
    if self._proc is not None and self._proc.returncode is not None:
        returncode = self._proc.returncode
    _process_exited(self, returncode)


asyncio.base_subprocess.BaseSubprocessTransport._process_exited = _keep_polled_status
