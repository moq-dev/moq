import os
import sys

# Act like a compiler whose heap leaves too little memory for the inputs it keeps
# re-reading: fill anonymous memory until 3% remains available, then read a file
# larger than that in a loop. The kernel can always evict those clean pages, so
# its own OOM killer never fires and every pass re-reads the disk.
inputs = sys.argv[1]
chunk = 8 << 20

with open(inputs, "wb") as f:
    for _ in range((128 << 20) // chunk):
        f.write(os.urandom(chunk))
    f.flush()
    os.fsync(f.fileno())
    os.posix_fadvise(f.fileno(), 0, 0, os.POSIX_FADV_DONTNEED)


def meminfo(key):
    with open("/proc/meminfo") as lines:
        for line in lines:
            if line.startswith(key + ":"):
                return int(line.split()[1])


heap = []
while meminfo("MemAvailable") > meminfo("MemTotal") * 3 // 100:
    heap.append(bytearray(b"\1" * chunk))
print(f"holding {len(heap) * chunk >> 20} MiB", flush=True)

while True:
    with open(inputs, "rb", buffering=0) as f:
        while f.read(1 << 20):
            pass
