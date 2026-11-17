"""Compares two per-frame DTX files (1 when no audio was sent): the port, then libopus.

Usage: python3 cmpdtx.py <port.txt> <ref.txt>
"""

import sys
a=[int(l) for l in open(sys.argv[1])]; b=[int(l) for l in open(sys.argv[2])]
n=min(len(a),len(b)); diff=[i for i in range(n) if a[i]!=b[i]]
print(f"frames={n} port-shut={sum(a)} libopus-shut={sum(b)} differ={len(diff)}", diff[:10])
