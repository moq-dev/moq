#!/usr/bin/env python3
"""Print a process birth identity with kernel-native resolution."""

import ctypes
import pathlib
import sys


def linux_start(pid: int) -> str:
    stat = pathlib.Path(f"/proc/{pid}/stat").read_text()
    fields = stat[stat.rfind(")") + 2 :].split()
    boot_id = pathlib.Path("/proc/sys/kernel/random/boot_id").read_text().strip()
    return f"linux:{boot_id}:{pid}:{fields[19]}"


class ProcBsdInfo(ctypes.Structure):
    _fields_ = [
        ("pbi_flags", ctypes.c_uint32),
        ("pbi_status", ctypes.c_uint32),
        ("pbi_xstatus", ctypes.c_uint32),
        ("pbi_pid", ctypes.c_uint32),
        ("pbi_ppid", ctypes.c_uint32),
        ("pbi_uid", ctypes.c_uint32),
        ("pbi_gid", ctypes.c_uint32),
        ("pbi_ruid", ctypes.c_uint32),
        ("pbi_rgid", ctypes.c_uint32),
        ("pbi_svuid", ctypes.c_uint32),
        ("pbi_svgid", ctypes.c_uint32),
        ("rfu_1", ctypes.c_uint32),
        ("pbi_comm", ctypes.c_char * 16),
        ("pbi_name", ctypes.c_char * 32),
        ("pbi_nfiles", ctypes.c_uint32),
        ("pbi_pgid", ctypes.c_uint32),
        ("pbi_pjobc", ctypes.c_uint32),
        ("e_tdev", ctypes.c_uint32),
        ("e_tpgid", ctypes.c_uint32),
        ("pbi_nice", ctypes.c_int32),
        ("pbi_start_tvsec", ctypes.c_uint64),
        ("pbi_start_tvusec", ctypes.c_uint64),
    ]


def darwin_start(pid: int) -> str:
    info = ProcBsdInfo()
    libproc = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    size = libproc.proc_pidinfo(pid, 3, 0, ctypes.byref(info), ctypes.sizeof(info))
    if size != ctypes.sizeof(info):
        raise ProcessLookupError(pid)
    return f"darwin:{pid}:{info.pbi_start_tvsec}:{info.pbi_start_tvusec}"


def main() -> None:
    pid = int(sys.argv[1])
    if sys.platform == "linux":
        identity = linux_start(pid)
    elif sys.platform == "darwin":
        identity = darwin_start(pid)
    else:
        raise RuntimeError(f"unsupported platform: {sys.platform}")
    print(identity)


if __name__ == "__main__":
    main()
