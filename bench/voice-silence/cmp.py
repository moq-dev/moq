"""Compares two per-frame activity probability files (the port, then libopus): the largest
difference and the frames whose decision at 0.1 differs.

Usage: python3 cmp.py <port.txt> <ref.txt>
"""

import sys
a=[l.strip() for l in open(sys.argv[1])]; b=[l.strip() for l in open(sys.argv[2])]
n=min(len(a),len(b)); maxd=0; flips=0; inv=0
for i in range(n):
    if (a[i]=="invalid")!=(b[i]=="invalid"): inv+=1; continue
    if a[i]=="invalid": continue
    x,y=float(a[i]),float(b[i]); d=abs(x-y); maxd=max(maxd,d)
    if (x>=0.1)!=(y>=0.1): flips+=1; print("flip",i,x,y)
print(f"frames={n} len {len(a)}/{len(b)} invalid-mismatch={inv} max|d|={maxd:.3g} decision flips={flips}")
