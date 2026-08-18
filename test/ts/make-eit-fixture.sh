#!/usr/bin/env bash
# Add a synthetic DVB EPG (EIT) to any transport stream.
#
# The MPEG-TS import path routes SI by PID, but no capture in this repository carries
# EIT (PID 0x0012), so nothing exercises that route. This synthesises one from a clip
# that has none, including the ffmpeg-generated clip run.sh makes. This makes the EIT path
# testable without needing a broadcast capture.
#
# Everything is derived from the input: the service triplet comes from its PAT and SDT, so
# the generated EIT actually describes the service the stream carries rather than a
# plausible-looking one a receiver would ignore. The EPG is anchored to the input's own
# TDT where it has one, and to a fixed date otherwise, so the output is byte-reproducible
# for a given input either way rather than varying with the wall clock.
#
# Requires TSDuck (tsp, tstables) and python3.
#
# Usage: make-eit-fixture.sh [options] <input.ts> <output.ts>
#
#   --pf-only        EIT p/f actual only. The default also generates EIT schedule.
#   --events N       events in the EPG (default 12)
#   --days N         span the EPG N days instead, at the same 30-minute events. This is
#                    the flag to reach for when the sparseness of EIT schedule matters:
#                    a guide at the DVB planning horizon declares a
#                    last_section_number covering its whole range and transmits only the
#                    segment-boundary sections that hold events, so completeness cannot
#                    be decided by counting. See "EIT fixtures" in README.md.
#   --time T         UTC reference, "YYYY/MM/DD:hh:mm:ss". Defaults to the input's own
#                    TDT if it has one, else a fixed date.
#   --service-id N   service to describe (default: the first service in the PAT)
#   --quiet          suppress the post-generation summary

set -euo pipefail

PF_ONLY=""
EVENTS=12
DAYS=""
TIME_REF=""
SERVICE_ID=""
QUIET=""
ARGS=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --pf-only)
            PF_ONLY=1
            shift
            ;;
        --events)
            EVENTS="$2"
            shift 2
            ;;
        --days)
            DAYS="$2"
            shift 2
            ;;
        --time)
            TIME_REF="$2"
            shift 2
            ;;
        --service-id)
            SERVICE_ID="$2"
            shift 2
            ;;
        --quiet)
            QUIET=1
            shift
            ;;
        -h | --help)
            sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            ARGS+=("$1")
            shift
            ;;
    esac
done

# A day of 30-minute events is 48 of them, so the horizon and the event count are the
# same knob; --days exists because the interesting property (how sparse the schedule
# sub-tables come out) is a function of the span, not of the count.
if [[ -n "$DAYS" ]]; then
    [[ "$DAYS" =~ ^[1-9][0-9]*$ ]] || {
        echo "error: --days takes a positive integer: $DAYS" >&2
        exit 2
    }
    EVENTS=$((DAYS * 48))
fi
[[ "$EVENTS" =~ ^[1-9][0-9]*$ ]] || {
    echo "error: --events takes a positive integer: $EVENTS" >&2
    exit 2
}

[[ ${#ARGS[@]} -eq 2 ]] || {
    echo "usage: $(basename "$0") [options] <input.ts> <output.ts>" >&2
    exit 2
}
IN="${ARGS[0]}"
OUT="${ARGS[1]}"
[[ -r "$IN" ]] || {
    echo "error: no such input: $IN" >&2
    exit 1
}

for t in tsp tstables python3; do
    command -v "$t" >/dev/null 2>&1 || {
        echo "error: missing required tool: $t" >&2
        exit 1
    }
done

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Read the service triplet from the stream rather than assuming it. An EIT whose
# (original_network_id, transport_stream_id, service_id) does not match the SDT describes
# nothing, and a receiver is right to ignore it.
tstables "$IN" --pid 0x0000 --pid 0x0011 --pid 0x0014 --max-tables 8 \
    --json-output "$TMP/si.json" --no-pager >/dev/null

METADATA=$(
    python3 - "$TMP/si.json" "$SERVICE_ID" <<'PY'
import json, sys

path, requested_sid = sys.argv[1:3]


def fail(message):
    print(f"error: {message}", file=sys.stderr)
    sys.exit(1)


def integer(value, name):
    if isinstance(value, bool):
        fail(f"invalid {name} in SI metadata: {value!r}")
    try:
        return int(value)
    except (TypeError, ValueError):
        fail(f"invalid {name} in SI metadata: {value!r}")


try:
    with open(path) as stream:
        doc = json.load(stream)
except (OSError, json.JSONDecodeError) as error:
    fail(f"cannot read complete PAT/SDT metadata: {error}")

tables = doc.get("#nodes", []) if isinstance(doc, dict) else doc
if not isinstance(tables, list):
    fail("PAT/SDT metadata is not a table list")

pats = [table for table in tables if table.get("#name") == "PAT"]
sdts = [
    table
    for table in tables
    if table.get("#name") == "SDT" and table.get("actual", True)
]
if not pats:
    fail("input has no complete PAT")
if not sdts:
    fail("input has no complete actual SDT")

pat_tsids = {integer(table.get("transport_stream_id"), "PAT transport_stream_id") for table in pats}
sdt_tsids = {integer(table.get("transport_stream_id"), "SDT transport_stream_id") for table in sdts}
if len(pat_tsids) != 1 or len(sdt_tsids) != 1 or pat_tsids != sdt_tsids:
    fail(f"PAT/SDT transport_stream_id mismatch: PAT {sorted(pat_tsids)}, SDT {sorted(sdt_tsids)}")
tsid = pat_tsids.pop()

onids = {integer(table.get("original_network_id"), "original_network_id") for table in sdts}
if len(onids) != 1:
    fail(f"actual SDT tables disagree on original_network_id: {sorted(onids)}")
onid = onids.pop()

pat_services = [
    integer(node.get("service_id"), "PAT service_id")
    for table in pats
    for node in table.get("#nodes", [])
    if node.get("#name") == "service"
]
sdt_services = {
    integer(node.get("service_id"), "SDT service_id")
    for table in sdts
    for node in table.get("#nodes", [])
    if node.get("#name") == "service"
}
if requested_sid:
    try:
        sid = int(requested_sid, 0)
    except ValueError:
        fail(f"invalid --service-id: {requested_sid}")
    if sid not in sdt_services:
        fail(f"service {sid} is not present in the actual SDT")
    if sid not in pat_services:
        fail(f"service {sid} is not present in the PAT")
else:
    try:
        sid = next(service for service in pat_services if service in sdt_services)
    except StopIteration:
        fail("PAT and actual SDT have no service in common")

tdt = "-"
for t in tables:
    if t.get("#name") == "TDT" and tdt == "-":
        raw = t.get("utc_time") or t.get("UTC_time") or ""
        # "YYYY-MM-DD hh:mm:ss" -> the "YYYY/MM/DD:hh:mm:ss" eitinject --time wants.
        if len(raw) == 19 and raw[4] == "-" and raw[10] == " ":
            tdt = raw.replace("-", "/").replace(" ", ":")
print(sid, tsid, onid, tdt)
PY
)

read -r SERVICE_ID TSID ONID TDT <<<"$METADATA"

# Anchor to the stream's own clock where it has one, so the "present" event is genuinely
# present for a receiver decoding from the start. Otherwise a fixed date, which keeps the
# output reproducible; eitinject needs a reference either way and would otherwise take the
# wall clock, making every run differ.
if [[ -z "$TIME_REF" ]]; then
    if [[ "$TDT" != "-" && -n "$TDT" ]]; then
        TIME_REF="$TDT"
    else
        TIME_REF="2026/01/01:12:00:00"
    fi
fi

python3 - "$TMP/epg.xml" "$SERVICE_ID" "$TSID" "$ONID" "$TIME_REF" "$EVENTS" <<'PY'
import sys, datetime

path, sid, tsid, onid, ref, count = sys.argv[1:7]
sid, tsid, onid, count = int(sid), int(tsid), int(onid), int(count)
t0 = datetime.datetime.strptime(ref, "%Y/%m/%d:%H:%M:%S")

# The first event has to straddle the reference, not merely precede it: an event ending
# exactly at the reference is already obsolete and eitinject drops it, leaving p/f empty
# for the whole clip.
start = t0 - datetime.timedelta(minutes=15)
titles = [
    ("News", "International news, business and weather."),
    ("Sport", "The day in sport."),
    ("Documentary", "Long-form reporting."),
    ("Talk", "Interviews with newsmakers."),
    ("Markets", "Companies and the people who run them."),
    ("Weather", "The outlook, region by region."),
]

rows = []
for i in range(count):
    s = start + datetime.timedelta(minutes=30 * i)
    name, text = titles[i % len(titles)]
    rows.append(
        f'    <event event_id="{1000 + i}" start_time="{s:%Y-%m-%d %H:%M:%S}" '
        f'duration="00:30:00" running_status="{"running" if i == 0 else "not-running"}" '
        f'CA_mode="false">\n'
        f'      <short_event_descriptor language_code="eng">\n'
        f'        <event_name>{name} {i + 1}</event_name>\n'
        f'        <text>{text}</text>\n'
        f"      </short_event_descriptor>\n"
        f"    </event>"
    )

# eitinject ignores this structure and re-derives p/f and schedule from the event list, so
# one EIT carrying every event is the intended input shape.
open(path, "w").write(
    '<?xml version="1.0" encoding="UTF-8"?>\n<tsduck>\n'
    f'  <EIT type="pf" actual="true" version="1" current="true"\n'
    f'       service_id="{sid}" transport_stream_id="{tsid}" original_network_id="{onid}">\n'
    + "\n".join(rows)
    + "\n  </EIT>\n</tsduck>\n"
)
PY

EIT_ARGS=(--actual)
[[ -n "$PF_ONLY" ]] && EIT_ARGS=(--actual-pf)

# tsp packet processors replace packets, they do not create them, so EIT can only go where
# there is stuffing. A broadcast capture has plenty and keeps its mux rate; a clip muxed by
# ffmpeg has none, so pad it to a constant rate first and say so, since that changes the
# stream in a way the caller should know about.
SRC="$IN"
NULLS=$(tsp -I file "$IN" -P count --pid 0x1FFF --total -O drop 2>&1 |
    sed -n 's/.*counted \([0-9,]*\) packets out of \([0-9,]*\).*/\1 \2/p' | head -1)
HAVE=$(echo "$NULLS" | cut -d' ' -f1 | tr -d ,)
TOTAL=$(echo "$NULLS" | cut -d' ' -f2 | tr -d ,)
if [[ -z "$HAVE" || -z "$TOTAL" || "$TOTAL" -eq 0 || $((HAVE * 200)) -lt "$TOTAL" ]]; then
    RATE=$(tsp -I file "$IN" -P analyze --normalized -O drop 2>/dev/null |
        sed -n 's/^ts:.*:bitrate=\([0-9]*\):.*/\1/p' | head -1)
    [[ -n "$RATE" && "$RATE" -gt 0 ]] || {
        echo "error: cannot determine input bitrate to pad to" >&2
        exit 1
    }
    PADDED=$((RATE + 200000))
    [[ -n "$QUIET" ]] || echo "### input carries no stuffing; padding to ${PADDED} b/s CBR to make room"
    tsstuff --bitrate "$PADDED" "$IN" >"$TMP/padded.ts" 2>"$TMP/stuff.log" || {
        sed 's/^/  tsstuff: /' "$TMP/stuff.log" >&2 || true
        exit 1
    }
    SRC="$TMP/padded.ts"
fi

# --wait-first-batch: without it injection races the EPG load and the head of the output
# carries no EIT, which a short capture would read as "the table was dropped".
# The SDT flags: a stream that carries EIT while advertising none is internally
# inconsistent, and a conformance analyser will say so.
tsp -I file "$SRC" \
    -P sdt --service-id "$SERVICE_ID" --eit-pf 1 --eit-schedule "$([[ -n "$PF_ONLY" ]] && echo 0 || echo 1)" \
    -P eitinject --files "$TMP/epg.xml" --wait-first-batch --time "$TIME_REF" "${EIT_ARGS[@]}" \
    -O file "$OUT" 2>"$TMP/tsp.log" || {
    sed 's/^/  tsp: /' "$TMP/tsp.log" >&2 || true
    exit 1
}

tstables "$OUT" --pid 0x0012 --json-output "$TMP/eit.json" --no-pager >/dev/null
python3 - "$TMP/eit.json" "$SERVICE_ID" "$TSID" "$ONID" <<'PY'
import json, sys

path, sid, tsid, onid = sys.argv[1:5]
expected = tuple(map(int, (sid, tsid, onid)))
try:
    with open(path) as stream:
        doc = json.load(stream)
except (OSError, json.JSONDecodeError) as error:
    print(f"error: cannot read generated EIT metadata: {error}", file=sys.stderr)
    sys.exit(1)

tables = doc.get("#nodes", []) if isinstance(doc, dict) else doc
observed = {
    (
        table.get("service_id"),
        table.get("transport_stream_id"),
        table.get("original_network_id"),
    )
    for table in tables
    if table.get("#name") == "EIT"
}
if expected not in observed:
    print(
        f"error: no generated EIT for service triplet {expected}; observed {sorted(observed)}",
        file=sys.stderr,
    )
    sys.exit(1)
PY

if [[ -z "$PF_ONLY" ]]; then
    python3 - "$OUT" "$EVENTS" <<'PY'
import collections, sys

path, events = sys.argv[1:3]
packet_size = 188


def fail(message):
    print(f"error: {message}", file=sys.stderr)
    sys.exit(1)


try:
    with open(path, "rb") as stream:
        data = stream.read()
except OSError as error:
    fail(f"cannot read generated transport stream: {error}")

if not data or len(data) % packet_size:
    fail("generated output is not a 188-byte-aligned transport stream")

schedule = collections.defaultdict(set)
last_sections = collections.defaultdict(set)
for off in range(0, len(data), packet_size):
    packet = data[off : off + packet_size]
    if packet[0] != 0x47:
        fail(f"generated output lost packet sync at byte {off}")
    if ((packet[1] & 0x1F) << 8 | packet[2]) != 0x0012 or not packet[1] & 0x40:
        continue
    if not packet[3] & 0x10:
        continue
    body = 4 + (1 + packet[4] if packet[3] & 0x20 else 0)
    if body >= packet_size:
        continue
    section = body + 1 + packet[body]
    while section + 3 <= packet_size and packet[section] != 0xFF:
        table_id = packet[section]
        length = 3 + ((packet[section + 1] & 0x0F) << 8 | packet[section + 2])
        # A start whose 8-byte header crosses into the next packet is legal;
        # skipping it undercounts distinct sections, which only makes the
        # sparseness check below more conservative.
        if 0x50 <= table_id <= 0x6F and section + 8 <= packet_size:
            schedule[table_id].add(packet[section + 6])
            last_sections[table_id].add(packet[section + 7])
        if section + length > packet_size:
            break
        section += length

if not schedule:
    fail("generated output has no EIT schedule section on PID 0x0012")
if int(events) > 48 and not any(
    last > len(schedule[table_id])
    for table_id, declared in last_sections.items()
    for last in declared
):
    census = ", ".join(
        f"0x{table_id:02X}: {len(sections)} distinct, last {sorted(last_sections[table_id])}"
        for table_id, sections in sorted(schedule.items())
    )
    fail(f"multi-day EIT schedule is not sparse: {census}")
PY
fi

EIT_PKTS=$(tsp -I file "$OUT" -P count --pid 0x0012 --total -O drop 2>&1 |
    sed -n 's/.*counted \([0-9,]*\) packets.*/\1/p' | head -1)

[[ -n "$QUIET" ]] || {
    echo "### EIT fixture: service $SERVICE_ID (ts $TSID, onid $ONID), reference $TIME_REF"
    echo "### $EVENTS events${DAYS:+ spanning $DAYS day(s)}, $([[ -n "$PF_ONLY" ]] && echo "p/f only" || echo "p/f + schedule")"
    echo "### $EIT_PKTS packets on PID 0x0012 -> $OUT"
}
