#!/usr/bin/env bash
# Round-trip the EIT fixtures through a relay and assert the SI carriage
# behavior they exist to pin: the sparse schedule survives (committed on cycle
# wrap, since its declared last_section_number can never be reached by
# counting), and pending-version sections (current_next_indicator clear) are
# dropped at import while the current ones keep round-tripping.
#
# Builds a broadcast-like clip, layers a two-day EPG onto it, rewrites the tail
# of the p/f table to a pending version, round-trips the result via run.sh, and
# censuses the capture against the source. Everything asserted here has a
# positive control: an importer that dropped EIT wholesale fails the p/f check,
# one that stored pending sections fails the pending check, and one that waited
# for schedule contiguity fails the schedule check.
#
# Usage: eit-roundtrip.sh [run.sh passthrough args]

set -euo pipefail

DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

DURATION="${TSC_EIT_DURATION:-20}"

# The same broadcast-like clip run.sh generates: real SDT, PCR, one-second GOP.
echo "### generating ~${DURATION}s clip for the EIT round-trip"
ffmpeg -y -hide_banner -loglevel error \
    -f lavfi -i "testsrc=size=1280x720:rate=25" \
    -f lavfi -i "sine=frequency=1000:sample_rate=48000" \
    -t "$DURATION" \
    -c:v libx264 -profile:v high -preset veryfast -pix_fmt yuv420p \
    -x264-params "keyint=25:min-keyint=25:scenecut=0" -b:v 8M \
    -c:a aac -b:a 128k \
    -f mpegts -pes_payload_size 0 "$TMP/clip.ts"

# Two days of guide: enough for the sparse shape (the generator's census
# enforces it) at a fraction of the eight-day fixture's size.
"$DIR/make-eit-fixture.sh" --days 2 --quiet "$TMP/clip.ts" "$TMP/eit.ts"
"$DIR/make-pending-eit.py" --quiet "$TMP/eit.ts" "$TMP/pending.ts"

"$DIR/run.sh" --source "$TMP/pending.ts" --duration "$DURATION" \
    --capture-out "$TMP/capture.ts" "$@"

echo
echo "### censusing the capture against the source"
python3 - "$TMP/pending.ts" "$TMP/capture.ts" <<'PY'
import sys

PACKET = 188


def census(path):
    """Current and pending EIT section headers, from every section start.

    Header tuples only (table_id, version, section_number, last_section_number,
    length): a schedule section's body spans packets, but its identity and shape
    live in the 8 header bytes at the start, which the packetizer never splits
    on the paths under test. A start whose header does cross is skipped; that
    can only under-count, and the source drives both sides identically.
    """
    with open(path, "rb") as stream:
        data = stream.read()
    if not data or len(data) % PACKET:
        print(f"error: {path} is not a 188-byte-aligned transport stream", file=sys.stderr)
        sys.exit(1)
    current, pending = set(), set()
    for off in range(0, len(data), PACKET):
        pkt = data[off : off + PACKET]
        if pkt[0] != 0x47 or ((pkt[1] & 0x1F) << 8 | pkt[2]) != 0x0012 or not pkt[1] & 0x40:
            continue
        if not pkt[3] & 0x10:
            continue
        body = 4 + (1 + pkt[4] if pkt[3] & 0x20 else 0)
        if body >= PACKET:
            continue
        sec = body + 1 + pkt[body]
        while sec + 3 <= PACKET and pkt[sec] != 0xFF:
            length = 3 + ((pkt[sec + 1] & 0x0F) << 8 | pkt[sec + 2])
            if sec + 8 <= PACKET and 0x4E <= pkt[sec] <= 0x6F:
                entry = (pkt[sec], (pkt[sec + 5] >> 1) & 0x1F, pkt[sec + 6], pkt[sec + 7], length)
                (current if pkt[sec + 5] & 1 else pending).add(entry)
            if sec + length > PACKET:
                break
            sec += length
    return current, pending


source_current, source_pending = census(sys.argv[1])
egress_current, egress_pending = census(sys.argv[2])

failures = []

pf = {e for e in source_current if e[0] == 0x4E}
if not pf:
    failures.append("source carries no current p/f: the fixture itself is broken")
if not pf <= egress_current:
    failures.append(f"current p/f lost in the round-trip: missing {sorted(pf - egress_current)}")

schedule = {e for e in source_current if 0x50 <= e[0] <= 0x6F}
if not schedule:
    failures.append("source carries no schedule: the fixture itself is broken")
if not any(last + 1 > sum(1 for s in schedule if s[0] == tid) for tid, _, _, last, _ in schedule):
    failures.append("source schedule is not sparse: the census would prove nothing")
if not schedule <= egress_current:
    failures.append(f"sparse schedule lost in the round-trip: missing {sorted(schedule - egress_current)}")

if not source_pending:
    failures.append("source carries no pending sections: the fixture itself is broken")
if egress_pending:
    failures.append(f"pending sections leaked to egress: {sorted(egress_pending)}")

if failures:
    for failure in failures:
        print(f"error: {failure}", file=sys.stderr)
    sys.exit(1)

print(
    f"### EIT round-trip: {len(pf)} p/f + {len(schedule)} sparse schedule section(s) "
    f"survived; {len(source_pending)} pending section(s) dropped at import"
)
PY
