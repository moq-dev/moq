import time

# Fill memory like a compiler's heap until 3% remains available, then hold it.
# That stays just short of the kernel's own OOM killer, which never fires.
chunk = b"\1" * (8 << 20)
heap = []


def meminfo(key):
    with open("/proc/meminfo") as lines:
        for line in lines:
            if line.startswith(key + ":"):
                return int(line.split()[1])


while True:
    if meminfo("MemAvailable") > meminfo("MemTotal") * 3 // 100:
        heap.append(bytearray(chunk))
    else:
        time.sleep(1)
