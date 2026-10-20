#!/usr/bin/env bash
# MPEG-TS / IRD compliance harness for the moq subscriber's `export ts` output.
#
# It stands up a moq-relay built from this checkout, publishes a PCR-paced
# transport stream (`tsp -P regulate | moq ... import ts`), captures the
# round-tripped stream from a second client (`moq ... export ts`), and runs the
# TSDuck + custom analyzer in compliance.py against the capture. The point is to
# tell whether what the subscriber emits is something an Integrated
# Receiver/Decoder would accept, and to quantify where it diverges (the exporter
# pads to the recorded multiplex rate but never delays media to fit it, and puts a
# PCR every 25 ms of media time).
#
# Modes:
#   ./run.sh                       # generate a clip, round-trip it, analyze
#   ./run.sh --source cap.ts       # round-trip a real capture instead
#   ./run.sh --analyze-only cap.ts # skip the round-trip, just analyze a file
#   ./run.sh --strict              # fail on broadcast-shape warnings too
#   ./run.sh --with-eit            # add a synthetic EPG first, report which SI survived
#   ./run.sh --live                # grade PCR release timing off the live pipe
#   ./run.sh --pair                # two exporters of one broadcast, grade table anchoring
#   ./run.sh --open-gop            # open-GOP clip; orphan leading pictures are trimmed
#   ./run.sh --hrd                 # 1080p video filling a broadcast-sized 9 Mbit CPB
#   ./run.sh --headroom            # the same with four audio PIDs, muxed above the video's Rx
#   ./run.sh --leak                # 1080p video with no HRD, filling its level's 24 Mb/s leak
#   ./run.sh --full                # the same with two audio PIDs, filling its multiplex with no nulls
#   ./run.sh --fragment-pes        # split generated clear PES headers across packets
#   ./run.sh --klv                 # add KLV and DVB AC-3, carried as whole PES, and grade them
#   ./run.sh --delay 2s            # pass the exporter's --delay
#   ./run.sh --rate 7000000        # bufferbloat: 7 Mb/s to the subscriber behind a deep queue (moq-shaper)
#   ./run.sh --loss 10             # drop 10% of the relay's datagrams to the subscriber (moq-shaper)
#   ./run.sh --passthrough         # carry the multiplex whole (`import/export ts --passthrough`)
#   ./run.sh --passthrough --mpts  # the same with two programs whose recovery points rarely line up

# `--live` swaps the analyzer, not the rig. compliance.py grades a captured file
# on the stream's own PCR clock, which is the right basis for the IRD model it
# builds and is why it needs no wall-clock capture -- and also why nothing it
# checks can see *when* the exporter handed the bytes over. pcr-timing.py reads
# the subscriber's stdout a packet at a time and stamps each read, so it grades
# release timing and byte position alongside the values. Nightly runs this arm
# (.github/workflows/ts.yml); it is not a per-PR gate, because it needs a
# real-time window to measure at all.
#
# `--pair` changes the rig rather than the analyzer: it subscribes twice to one
# broadcast, the second joining late, and grades the two captures against each
# other with table-anchor.py. One exporter cannot show whether a table's emission
# points belong to the broadcast or to the process that happened to be running,
# because there is nothing to disagree with -- so this is the only arm that can
# see a cadence regression at all.
set -euo pipefail

DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$DIR/../.." && pwd)

# Run directory, reserved ports, and process-group ownership. See test/README.md.
# shellcheck source-path=SCRIPTDIR source=../lib/harness.sh
source "$DIR/../lib/harness.sh"
# shellcheck source-path=SCRIPTDIR source=process.sh
source "$DIR/process.sh"

# Captured before the parse below consumes it, so the rerun command carries the
# source capture, mux rate, EIT fixture, and analyzer thresholds this run used,
# whether they arrived as flags or as environment overrides.
RERUN="$(harness_env TSC_DURATION TSC_BITRATE TSC_PORT TSC_PROFILE TSC_SEED MOQ_QUIC_CONGESTION_CONTROL)just test ts$(harness_argv "$@")"

# Seamless session replacement needs an explicit publisher epoch. Keep this
# opt-in local to the validation rig; epochless replacement is tested as refusal.
VERSION=moq-lite-07-wip

SOURCE=""       # real capture to publish instead of a generated clip
ANALYZE_ONLY="" # existing TS to analyze without a round-trip
CAPTURE_OUT=""  # keep the subscriber capture here (for callers asserting on it)
DURATION="${TSC_DURATION:-20}"
BITRATE="${TSC_BITRATE:-10000000}"
# Empty means "any reserved port"; TSC_PORT or --port pins one instead.
PORT="${TSC_PORT:-}"
PROFILE="${TSC_PROFILE:-debug}"
STRICT=""
WITH_EIT=""     # add a synthetic EPG to the source and report which SI survived
LIVE=""         # grade the exporter's stdout as it arrives, rather than a capture
PAIR=""         # subscribe twice and grade the two captures against each other
OPEN_GOP=""     # publish open GOP with leading pictures, and grade them through the round-trip
HRD=""          # publish video that fills a broadcast-sized CPB, as a contribution encoder does
HEADROOM=""     # the same plus four audio PIDs, in a multiplex faster than the video's Rx
LEAK=""         # publish video with no HRD at its level's leak, as a full-rate feed does
FULL=""         # the same with audio, in a multiplex the source fills with no nulls
FRAGMENT_PES="" # use following null slots for fragmented PES headers
KLV=""          # inject asynchronous and synchronous KLV beside DVB AC-3, and grade every PES
LOSS=""         # percent of relay -> subscriber datagrams dropped at random by moq-shaper
RATE=""         # bits/s moq-shaper carries from the relay to the subscriber, behind QUEUE
QUEUE=""        # how long a datagram may wait for RATE before the path tail-drops it
PASSTHROUGH=""  # carry the multiplex whole and grade the capture packet by packet against the source
MPTS=""         # publish two programs, which only --passthrough carries whole
EXPORT=()       # extra `export ts` flags (--delay)
# How far into the run the second subscriber joins. A late join is the point: two
# exporters started together can share a cadence by starting together, which is
# exactly the thing under test.
PAIR_JOIN="${TSC_PAIR_JOIN:-5}"
# Shortest overlap worth a verdict, in seconds. Below this the slower tables fall under
# the analyzer's emission floor and go report-only, which reads as a pass.
PAIR_MIN_OVERLAP="${TSC_PAIR_MIN_OVERLAP:-25}"
DURATION_SET="" # so pair and leak modes can raise the default without overriding an explicit --duration
BITRATE_SET=""  # so headroom and leak modes can raise the default without overriding an explicit --bitrate
PASSTHRU=()     # forwarded to compliance.py (thresholds, --report-json, ...)

while [[ $# -gt 0 ]]; do
    case "$1" in
        --source)
            SOURCE="$2"
            shift 2
            ;;
        --analyze-only)
            ANALYZE_ONLY="$2"
            shift 2
            ;;
        --duration)
            DURATION="$2"
            DURATION_SET=1
            shift 2
            ;;
        --bitrate)
            BITRATE="$2"
            BITRATE_SET=1
            shift 2
            ;;
        --port)
            PORT="$2"
            shift 2
            ;;
        --strict)
            STRICT="--strict"
            shift
            ;;
        --capture-out)
            CAPTURE_OUT="$2"
            shift 2
            ;;
        --with-eit)
            WITH_EIT=1
            shift
            ;;
        --live)
            LIVE=1
            shift
            ;;
        --pair)
            PAIR=1
            shift
            ;;
        --pair-join)
            PAIR_JOIN="$2"
            shift 2
            ;;
        --open-gop)
            OPEN_GOP=1
            shift
            ;;
        --hrd)
            HRD=1
            shift
            ;;
        --headroom)
            HEADROOM=1
            shift
            ;;
        --leak)
            LEAK=1
            shift
            ;;
        --full)
            FULL=1
            shift
            ;;
        --fragment-pes)
            FRAGMENT_PES=1
            shift
            ;;
        --klv)
            KLV=1
            shift
            ;;
        --delay)
            EXPORT=(--delay "$2")
            shift 2
            ;;
        --loss)
            LOSS="$2"
            shift 2
            ;;
        --rate)
            RATE="$2"
            shift 2
            ;;
        --queue)
            QUEUE="$2"
            shift 2
            ;;
        --passthrough)
            PASSTHROUGH=1
            shift
            ;;
        --mpts)
            MPTS=1
            shift
            ;;
        *)
            PASSTHRU+=("$1")
            shift
            ;;
    esac
done

if [[ -n "$PAIR" ]]; then
    if [[ -n "$LIVE" ]]; then
        echo "error: --live and --pair grade different things and cannot be combined" >&2
        echo "  --live grades one exporter's release timing; --pair grades two exporters against each other" >&2
        exit 1
    fi
    # The overlap, not the run, is what gets graded, and the default run is too short to
    # produce one worth grading: at 20s with a 5s join the legs share 15s, which is about
    # seven SDT emissions against a floor of eight, so the table this mode exists to check
    # would quietly drop to report-only. Give pair mode its own default and check the
    # arithmetic rather than letting a short window pass as a clean one.
    [[ -n "$DURATION_SET" ]] || DURATION=45
    if ((DURATION - PAIR_JOIN < PAIR_MIN_OVERLAP)); then
        echo "error: --pair needs at least ${PAIR_MIN_OVERLAP}s of overlap; this run has $((DURATION - PAIR_JOIN))s" >&2
        echo "  raise --duration above $((PAIR_JOIN + PAIR_MIN_OVERLAP)), or lower --pair-join" >&2
        exit 1
    fi
fi

# open-gop.py grades a capture against its source, which only the plain round-trip keeps.
if [[ -n "$HRD" && -n "$OPEN_GOP$SOURCE$ANALYZE_ONLY" ]]; then
    echo "error: --hrd generates its own clip and cannot be combined with --open-gop, --source, or --analyze-only" >&2
    exit 1
fi
if [[ -n "$HEADROOM" && -n "$HRD$OPEN_GOP$SOURCE$ANALYZE_ONLY" ]]; then
    echo "error: --headroom generates its own clip and cannot be combined with --hrd, --open-gop, --source, or --analyze-only" >&2
    exit 1
fi
# Room for the audio, with the video still taking up to 86 % of the multiplex.
[[ -n "$HEADROOM" && -z "$BITRATE_SET" && -z "${TSC_BITRATE:-}" ]] && BITRATE=12500000
if [[ -n "$LEAK" && -n "$HRD$HEADROOM$OPEN_GOP$SOURCE$ANALYZE_ONLY" ]]; then
    echo "error: --leak generates its own clip and cannot be combined with --hrd, --headroom, --open-gop, --source, or --analyze-only" >&2
    exit 1
fi
# Room for the video's packet headers, and long enough that an export admitting video a
# few percent below the leak exhausts the delay's lead and misses a deadline.
[[ -n "$LEAK" && -z "$BITRATE_SET" && -z "${TSC_BITRATE:-}" ]] && BITRATE=26000000
[[ -n "$LEAK" && -z "$DURATION_SET" && -z "${TSC_DURATION:-}" ]] && DURATION=30
if [[ -n "$FULL" && -n "$LEAK$HRD$HEADROOM$OPEN_GOP$SOURCE$ANALYZE_ONLY" ]]; then
    echo "error: --full generates its own clip and cannot be combined with --leak, --hrd, --headroom, --open-gop, --source, or --analyze-only" >&2
    exit 1
fi
# The clip's rate is whatever its packets fill, measured once it is generated.
if [[ -n "$FULL" && -n "$BITRATE_SET${TSC_BITRATE:-}" ]]; then
    echo "error: --full runs at the rate its clip fills and takes no --bitrate" >&2
    exit 1
fi
[[ -n "$FULL" && -z "$DURATION_SET" && -z "${TSC_DURATION:-}" ]] && DURATION=30
# Its video arrives up to about 0.65 s ahead of decoding with no room to catch up, so the
# export needs a longer lead than the default delay gives, yet one a unit's window and
# slot keep under the T-STD's 1 s.
[[ -n "$FULL" && ${#EXPORT[@]} -eq 0 ]] && EXPORT=(--delay 900ms)
if [[ -n "$OPEN_GOP" && -n "$ANALYZE_ONLY$LIVE$PAIR" ]]; then
    echo "error: --open-gop cannot be combined with --analyze-only, --live, or --pair" >&2
    exit 1
fi
# open-gop.py owes every leading picture, which a path that sheds groups cannot deliver.
if [[ -n "$OPEN_GOP" && -n "$RATE" ]]; then
    echo "error: --open-gop grades every leading picture and cannot be combined with --rate, which sheds groups" >&2
    exit 1
fi

if [[ -n "$FRAGMENT_PES" && -n "$SOURCE$ANALYZE_ONLY$HRD$HEADROOM$LEAK$FULL$OPEN_GOP$PAIR$LIVE" ]]; then
    echo "error: --fragment-pes requires the generated closed-GOP H.264/AAC profile" >&2
    exit 1
fi

if [[ -n "$KLV" && -n "$SOURCE$ANALYZE_ONLY$HRD$HEADROOM$LEAK$FULL$OPEN_GOP$FRAGMENT_PES$PAIR$LIVE$PASSTHROUGH$MPTS" ]]; then
    echo "error: --klv generates its own clip and grades the demultiplexed export's PES" >&2
    exit 1
fi

# An impaired path to the subscriber, 25 ms each way through moq-shaper, so it needs no
# privileges and leaves the host's network alone. `--rate` congests it the way real paths
# fail and is what judges the export; `--loss` drops datagrams at random (moq-dev/moq#4613),
# which real paths do not, so it only proves QUIC retransmission keeps the output whole.
SHAPED="$LOSS$RATE"
if [[ -n "$LOSS" && -n "$RATE$LIVE" ]]; then
    echo "error: --loss cannot be combined with --rate or --live; use --rate without random loss for quality grading" >&2
    exit 1
fi
if [[ -n "$SHAPED" && -n "$ANALYZE_ONLY$PAIR" ]]; then
    echo "error: --loss and --rate impair the round-trip's one subscriber and cannot be combined with --analyze-only or --pair" >&2
    exit 1
fi
if [[ -n "$LOSS" ]] && { ! [[ "$LOSS" =~ ^[0-9]+(\.[0-9]+)?$ ]] || ! awk -v p="$LOSS" 'BEGIN { exit !(p > 0 && p < 100) }'; }; then
    echo "error: --loss takes a percentage above 0 and below 100 (got '$LOSS')" >&2
    exit 1
fi
if [[ -n "$RATE" ]] && ! [[ "$RATE" =~ ^[1-9][0-9]*$ ]]; then
    echo "error: --rate takes bits per second (got '$RATE')" >&2
    exit 1
fi
if [[ -n "$QUEUE" && -z "$RATE" ]]; then
    echo "error: --queue is the depth behind --rate and needs it" >&2
    exit 1
fi
# A bloated buffer, not the shaper's 100 ms default: twice the export's 1 s delay, so a
# sender that kept it full would make every unit late and lose the tail holes.py requires,
# while it still tail-drops what overflows it. Measured on --hrd at 7 Mb/s, BBRv3 never
# filled one deeper than about a round trip: 50 ms overflowed hundreds of datagrams, 100 ms
# a few or none, 150 ms and deeper none.
[[ -n "$RATE" && -z "$QUEUE" ]] && QUEUE=2s

# The capture is the source's own packets, graded against it by passthrough.py; the arms that
# grade a second capture or the live pipe grade the demultiplexed export's choices.
if [[ -n "$PASSTHROUGH" && -n "$ANALYZE_ONLY$PAIR$LIVE" ]]; then
    echo "error: --passthrough cannot be combined with --analyze-only, --pair, or --live" >&2
    exit 1
fi
if [[ -n "$MPTS" && (-z "$PASSTHROUGH" || -n "$SOURCE$HRD$HEADROOM$LEAK$FULL$OPEN_GOP$FRAGMENT_PES") ]]; then
    echo "error: --mpts generates its own clip and needs --passthrough, the mode that carries a multiplex whole" >&2
    exit 1
fi
IMPORT=(import ts)
if [[ -n "$PASSTHROUGH" ]]; then
    IMPORT+=(--passthrough)
    EXPORT+=(--passthrough)
fi

URL="" # set once a port is reserved, below

have() { command -v "$1" >/dev/null 2>&1; }

require_tools() {
    local missing=() t
    for t in tsp tsanalyze tstables python3; do
        have "$t" || missing+=("$t")
    done
    # ffmpeg + cargo are only needed for the round-trip, not for --analyze-only.
    if [[ -z "$ANALYZE_ONLY" ]]; then
        for t in cargo ffmpeg ffprobe curl timeout; do have "$t" || missing+=("$t"); done
    fi
    # The EIT fixture reads the service triplet out of the stream and may need to pad a
    # stuffing-free clip to make room for the table.
    if [[ -n "$WITH_EIT" ]]; then
        for t in tstables tsstuff; do have "$t" || missing+=("$t"); done
    fi
    if [[ ${#missing[@]} -gt 0 ]]; then
        echo "error: missing required tools: ${missing[*]}" >&2
        echo "  TSDuck (tsp, tsanalyze, tstables) is required; install from https://tsduck.io" >&2
        exit 1
    fi
}

analyze() {
    # Single source of truth for the verdict: compliance.py runs the TSDuck
    # tools itself and prints the PASS/WARN/FAIL summary. A second argument is
    # the source TS, which enables the duration-fidelity check (round-trip only).
    local ref=() report=()
    [[ -n "${HARNESS_RUN:-}" ]] && report=(--report-json "$HARNESS_RUN/compliance.json")
    [[ -n "${2:-}" ]] && ref=(--reference "$2")
    python3 "$DIR/compliance.py" --ts "$1" ${ref[@]+"${ref[@]}"} ${report[@]+"${report[@]}"} $STRICT ${PASSTHRU[@]+"${PASSTHRU[@]}"}
}

require_tools

# ── analyze-only: no relay, no build ────────────────────────────────────────
if [[ -n "$ANALYZE_ONLY" ]]; then
    [[ -f "$ANALYZE_ONLY" ]] || {
        echo "error: no such file: $ANALYZE_ONLY" >&2
        exit 1
    }
    analyze "$ANALYZE_ONLY"
    exit $?
fi

# ── round-trip capture ──────────────────────────────────────────────────────
# Only the round-trip stands anything up, so `--analyze-only` above needs no run
# directory, no reserved port, and nothing to reap.
harness_begin ts "$RERUN"
ffmpeg -version >"$HARNESS_RUN/ffmpeg-version.txt"
ffprobe -version >"$HARNESS_RUN/ffprobe-version.txt"
tsp --version >"$HARNESS_RUN/tsduck-version.txt" 2>&1

TARGET_BASE=$(cargo metadata --format-version 1 --manifest-path "$WORKSPACE/Cargo.toml" --no-deps |
    sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
[[ -n "$TARGET_BASE" ]] || {
    echo "error: could not resolve cargo target directory" >&2
    exit 1
}

echo "### building moq-relay + moq-cli ($PROFILE)"
flag=()
[[ "$PROFILE" == "release" ]] && flag=(--release)
[[ -n "$SHAPED" ]] && flag+=(-p moq-shaper)
(cd "$WORKSPACE" && cargo build --locked ${flag[@]+"${flag[@]}"} -p moq-relay -p moq-cli)
RELAY="$TARGET_BASE/$PROFILE/moq-relay"
MOQ="$TARGET_BASE/$PROFILE/moq"
SHAPER="$TARGET_BASE/$PROFILE/moq-shaper"

BROADCAST="tscompliance-$$-${RANDOM}.hang"
SRC_TS="$HARNESS_RUN/source.ts"
SUB_TS="$HARNESS_RUN/sub.ts"
SUB_B_TS="$HARNESS_RUN/sub-b.ts"

# Source TS: a real capture (preserves all PIDs/PSI) or a generated broadcast-like
# clip (H.264 + AAC, one-second GOP, per-frame PES so audio interleaves evenly).
if [[ -n "$SOURCE" ]]; then
    [[ -f "$SOURCE" ]] || {
        echo "error: no such source: $SOURCE" >&2
        exit 1
    }
    echo "### cutting ~${DURATION}s from $SOURCE with TSDuck (all PIDs preserved)"
    PKTS=$((DURATION * BITRATE / 8 / 188))
    tsp -I file "$SOURCE" -P until --packets "$PKTS" -O file "$SRC_TS" 2>"$HARNESS_RUN/tsp-cut.log" || {
        sed 's/^/  tsp: /' "$HARNESS_RUN/tsp-cut.log" >&2 || true
        exit 1
    }
elif [[ -n "$HRD$HEADROOM" ]]; then
    echo "### generating ~${DURATION}s 1080p clip filling a 9 Mbit CPB${HEADROOM:+, with four audio PIDs,} with ffmpeg"
    # A contribution encoder's shape: CBR video whose HRD declares the whole buffer, kept
    # near full by noise, so the source sends pictures most of a second ahead of their
    # decode time and the export has to as well (moq-dev/moq#4645).
    INPUTS=(-f lavfi -i "testsrc2=size=1920x1080:rate=25,noise=alls=12:allf=t")
    AUDIO=(-an)
    if [[ -n "$HEADROOM" ]]; then
        # A broadcast feed's audio beside it: three MPEG-1 Layer II PIDs and an AC-3 one, in a
        # multiplex faster than the video's Rx (10.8 Mb/s). Only then can video packets arrive
        # faster than its transport buffer drains, so each slot has to interleave them with
        # the audio and the nulls (moq-dev/moq#5142). At 640 kb/s the AC-3 frame nearly fills
        # ATSC's 2,592-byte buffer, so the next frame needs its exact decode-time release.
        AUDIO=(-map 0:v)
        for k in 1 2 3 4; do
            INPUTS+=(-f lavfi -i "sine=frequency=$((330 * k)):sample_rate=48000")
            AUDIO+=(-map "$k:a")
        done
        AUDIO+=(-ac 2 -c:a mp2 -b:a 192k -c:a:3 ac3 -b:a:3 640k)
    fi
    ffmpeg -y -hide_banner -loglevel error \
        "${INPUTS[@]}" \
        -t "$DURATION" "${AUDIO[@]}" \
        -c:v libx264 -profile:v high -level 4.0 -preset veryfast -pix_fmt yuv420p \
        -x264-params "keyint=25:min-keyint=25:scenecut=0:nal-hrd=cbr" -b:v 9M -maxrate 9M -bufsize 9M \
        -f mpegts -muxrate "$BITRATE" -pcr_period 20 -pes_payload_size 0 "$SRC_TS"
elif [[ -n "$MPTS" ]]; then
    echo "### generating ~${DURATION}s two-program clip with ffmpeg"
    # Two services in one multiplex, each with its own PCR PID, their keyframes every 25 and
    # 40 frames: they coincide every 8 s, so most of the importer's groups, which open on
    # program 1's random access points, open mid-GOP for program 2.
    ffmpeg -y -hide_banner -loglevel error \
        -f lavfi -i "testsrc=size=640x360:rate=25" -f lavfi -i "sine=frequency=1000:sample_rate=48000" \
        -f lavfi -i "testsrc2=size=640x360:rate=25" -f lavfi -i "sine=frequency=500:sample_rate=48000" \
        -t "$DURATION" -map 0:v -map 1:a -map 2:v -map 3:a \
        -c:v libx264 -profile:v high -preset veryfast -pix_fmt yuv420p -b:v 2M \
        -x264-params:v:0 "keyint=25:min-keyint=25:scenecut=0" -x264-params:v:1 "keyint=40:min-keyint=40:scenecut=0" \
        -c:a aac -b:a 128k \
        -program program_num=1:st=0:st=1 -program program_num=2:st=2:st=3 \
        -f mpegts -muxrate "$BITRATE" -pcr_period 20 -pes_payload_size 0 "$SRC_TS"
elif [[ -n "$LEAK" ]]; then
    echo "### generating ~${DURATION}s 1080p clip with no HRD at its level's 24 Mb/s leak with ffmpeg"
    # A full-rate feed's shape: with no HRD, level 4.0's leak from MB into EB (Rbx, 24 Mb/s)
    # is the video's only limit, and a target above the VBV's 24 Mb/s ceiling holds the
    # video at it, so the export has to admit the leak's whole rate (moq-dev/moq#5362).
    ffmpeg -y -hide_banner -loglevel error \
        -f lavfi -i "testsrc2=size=1920x1080:rate=25,noise=alls=24:allf=t" \
        -t "$DURATION" -an \
        -c:v libx264 -profile:v high -level 4.0 -preset veryfast -pix_fmt yuv420p \
        -x264-params "keyint=25:min-keyint=25:scenecut=0" -b:v 30M -maxrate 24M -bufsize 24M \
        -f mpegts -muxrate "$BITRATE" -pcr_period 20 -pes_payload_size 0 "$SRC_TS"
elif [[ -n "$FULL" ]]; then
    echo "### generating ~${DURATION}s 1080p clip at its level's leak with two audio PIDs, filling its multiplex with no nulls"
    # A full-rate contribution feed's shape: the --leak video beside an MP2 and a DVB AC-3
    # PID, with its PCR in the video's adaptation field and several audio frames a PES, and
    # no null packets, so the export has no room to spend more packets than the source on
    # clocks or audio framing (moq-dev/moq#5362). ffmpeg pads any multiplex rate it is
    # given, so mux with room to spare, drop the nulls, and stamp the clock at the rate the
    # rest fill. The PTS and DTS stay: moving them with each PCR would move B pictures
    # against their group's keyframe. The video absorbs the time the nulls leave
    # (drop_nulls.py), since it goes out far enough ahead of its DTS for its packets to
    # arrive a little earlier or later: muxed at most 0.5 s ahead, it arrives up to about
    # 0.65 s ahead once the nulls are gone. The audio keeps ffmpeg's placement, which the
    # export keeps for the AC-3 it carries whole.
    PADDED_TS="$HARNESS_RUN/padded.ts"
    PADDED_RATE=27000000
    ffmpeg -y -hide_banner -loglevel error \
        -f lavfi -i "testsrc2=size=1920x1080:rate=25,noise=alls=24:allf=t" \
        -f lavfi -i "sine=frequency=440:sample_rate=48000" \
        -f lavfi -i "sine=frequency=880:sample_rate=48000" \
        -t "$DURATION" -map 0:v -map 1:a -map 2:a -ac 2 \
        -c:v libx264 -profile:v high -level 4.0 -preset veryfast -pix_fmt yuv420p \
        -x264-params "keyint=25:min-keyint=25:scenecut=0" -b:v 30M -maxrate 24M -bufsize 24M \
        -c:a:0 mp2 -b:a:0 192k -c:a:1 ac3 -b:a:1 192k \
        -f mpegts -mpegts_flags system_b -muxrate "$PADDED_RATE" -pcr_period 20 -max_delay 500000 "$PADDED_TS"
    PACKETS=$(($(wc -c <"$PADDED_TS") / 188))
    NULLS=$(tsp -I file "$PADDED_TS" -P count --pid 0x1FFF --total -O drop 2>&1 |
        sed -n 's/.*counted \([0-9,]*\) packets.*/\1/p' | tr -d ,)
    [[ -n "$NULLS" ]] || {
        echo "error: could not count the generated clip's null packets" >&2
        exit 1
    }
    BITRATE=$((PADDED_RATE * (PACKETS - NULLS) / PACKETS))
    DROPPED_TS="$HARNESS_RUN/dropped.ts"
    python3 "$DIR/drop_nulls.py" "$PADDED_TS" "$DROPPED_TS" --pid 0x100
    tsp -I file "$DROPPED_TS" -P pcradjust --bitrate "$BITRATE" --ignore-pts --ignore-dts -O file "$SRC_TS"
    echo "  ${NULLS} null packets dropped of ${PACKETS}: ${BITRATE} b/s"
elif [[ -n "$KLV" ]]; then
    echo "### generating ~${DURATION}s clip with DVB AC-3 and KLV"
    # The importer decodes neither DVB AC-3 (system_b's private data with an AC-3 descriptor)
    # nor KLV, so it carries each PES whole. FFmpeg's default PES size puts several AC-3 sync
    # frames in each, whose PTS times only the first: the export has to remove each from the
    # T-STD buffer at its own decode time. klv.py then fills null slots with KLV.
    ffmpeg -y -hide_banner -loglevel error \
        -f lavfi -i "testsrc=size=1280x720:rate=25" \
        -f lavfi -i "sine=frequency=1000:sample_rate=48000" \
        -f lavfi -i "sine=frequency=500:sample_rate=48000" \
        -t "$DURATION" -map 0:v -map 1:a -map 2:a -ac 2 \
        -c:v libx264 -profile:v high -preset veryfast -pix_fmt yuv420p \
        -x264-params "keyint=25:min-keyint=25:scenecut=0" -b:v 8M \
        -c:a:0 aac -b:a:0 128k -c:a:1 ac3 -b:a:1 192k \
        -f mpegts -mpegts_flags system_b -muxrate "$BITRATE" -pcr_period 20 "$HARNESS_RUN/media.ts"
    python3 "$DIR/klv.py" inject "$HARNESS_RUN/media.ts" "$SRC_TS" --report "$HARNESS_RUN/klv-injected.json"
else
    echo "### generating ~${DURATION}s broadcast-like ${OPEN_GOP:+open-GOP }clip with ffmpeg"
    # CBR with a 20 ms PCR, like a contribution feed. Not cosmetic: `regulate`
    # paces on the source PCR, so a clip whose clock is coarse and whose rate is
    # unconstrained is released unevenly and finishes early (measured: a 20 s clip
    # in 17 s, and release jitter of its own). The harness then grades ffmpeg.
    X264="keyint=25:min-keyint=25:scenecut=0"
    # Open GOP: after the first IDR, every keyframe is a non-IDR I picture with a
    # recovery-point SEI. Fixed B placement in a 24-frame GOP puts three B pictures
    # right after each one in decode order, presented before it: leading pictures.
    [[ -n "$OPEN_GOP" ]] && X264="keyint=24:min-keyint=24:scenecut=0:open-gop=1:bframes=3:b-adapt=0"
    ffmpeg -y -hide_banner -loglevel error \
        -f lavfi -i "testsrc=size=1280x720:rate=25" \
        -f lavfi -i "sine=frequency=1000:sample_rate=48000" \
        -t "$DURATION" \
        -c:v libx264 -profile:v high -preset veryfast -pix_fmt yuv420p \
        -x264-params "$X264" -b:v 8M \
        -c:a aac -b:a 128k \
        -f mpegts -muxrate "$BITRATE" -pcr_period 20 -pes_payload_size 0 "$SRC_TS"
fi

if [[ -n "$FRAGMENT_PES" ]]; then
    python3 "$DIR/fragment_pes.py" "$SRC_TS" "$HARNESS_RUN/source-fragmented.ts" \
        --report "$HARNESS_RUN/fragmented-pes.json"
    mv "$HARNESS_RUN/source-fragmented.ts" "$SRC_TS"
fi

# No capture in this repository carries EIT, so the import path's EIT handling is
# otherwise untestable. Synthesise one, and report below which SI PIDs came back.
if [[ -n "$WITH_EIT" ]]; then
    "$DIR/make-eit-fixture.sh" "$SRC_TS" "$HARNESS_RUN/source-eit.ts"
    mv "$HARNESS_RUN/source-eit.ts" "$SRC_TS"
fi

# Held for the rest of the run, so a concurrent harness cannot pick the same
# number between here and the relay's bind.
harness_port relay "$PORT"
PORT="$HARNESS_PORT"
URL="http://127.0.0.1:${PORT}"

# The reservation covers other harness runs, not the rest of the machine, so
# still refuse a port some unrelated process is already serving on. Without this
# the readiness probe below would be satisfied by that relay while ours died on
# its failed bind, and the round-trip would grade a binary nobody built here.
if harness_probe "$URL/certificate.sha256"; then
    echo "error: something is already listening on 127.0.0.1:${PORT} (stale relay?)" >&2
    exit 1
fi

echo "### starting relay on 127.0.0.1:${PORT} ($VERSION)"
printf '%s\n' "$VERSION" >"$HARNESS_RUN/moq-version.txt"
harness_spawn relay "$HARNESS_RUN/relay.log" "$RELAY" "$DIR/relay.toml" \
    --listen-version "$VERSION" --listen "127.0.0.1:${PORT}" --web-http-listen "127.0.0.1:${PORT}"
if ! harness_ready "$URL/certificate.sha256" 30 "$HARNESS_PID"; then
    echo "error: relay never became ready" >&2
    sed 's/^/  relay: /' "$HARNESS_RUN/relay.log" >&2 || true
    exit 1
fi
harness_endpoint relay "$URL"

# Subscribers dial the relay directly, or the shaper in front of it under --loss or --rate.
# The shaper carries only UDP, so the certificate's fingerprint comes from the relay itself.
SUB_CONNECT=(--connect "$URL")
if [[ -n "$SHAPED" ]]; then
    harness_port shaper
    SHAPER_PORT="$HARNESS_PORT"
    SEED="${TSC_SEED:-1}"
    PATH_NAME="ts${LOSS:+-loss-$LOSS}${RATE:+-rate-$RATE}"
    PATH_DESC="${LOSS:+${LOSS}% loss, }${RATE:+${RATE} b/s behind a ${QUEUE} queue, }25 ms each way"
    {
        printf 'name = "%s"\nseed = %s\n\n[up]\ndelay = "25ms"\n\n[down]\ndelay = "25ms"\n' "$PATH_NAME" "$SEED"
        if [[ -n "$LOSS" ]]; then
            awk -v p="$LOSS" 'BEGIN { printf "loss = %s\n", p / 100 }'
        fi
        # One datagram of burst, so the bucket paces a flight instead of letting it through whole.
        if [[ -n "$RATE" ]]; then
            printf '\n[down.rate]\nbits_per_second = %s\nburst = 1500\nqueue = "%s"\n' "$RATE" "$QUEUE"
        fi
    } >"$HARNESS_RUN/shaper.toml"
    echo "### starting moq-shaper on 127.0.0.1:${SHAPER_PORT}: ${PATH_DESC} to the subscriber, seed $SEED"
    harness_spawn shaper "$HARNESS_RUN/shaper.log" "$SHAPER" --listen "127.0.0.1:$SHAPER_PORT" \
        --target "127.0.0.1:$PORT" --profile "$HARNESS_RUN/shaper.toml" --report "$HARNESS_RUN/shaper.json"
    SHAPER_PID="$HARNESS_PID"
    # It prints its seed and profile once bound; a subscriber dialing before that loses its handshake.
    until grep -q '^shaper:' "$HARNESS_RUN/shaper.log" 2>/dev/null; do
        if harness_exited "$SHAPER_PID"; then
            echo "error: moq-shaper exited before binding" >&2
            sed 's/^/  shaper: /' "$HARNESS_RUN/shaper.log" >&2 || true
            exit 1
        fi
        sleep 0.05
    done
    # No WebSocket fallback: it would race the impaired QUIC path over TCP, which the shaper does not carry.
    FINGERPRINT=$(curl -fsS "$URL/certificate.sha256")
    SUB_CONNECT=(--connect "https://127.0.0.1:${SHAPER_PORT}" --connect-tls-fingerprint "$FINGERPRINT" --connect-websocket-enabled=false)
fi

# Start the subscriber first so it is waiting on the announce before the
# publisher appears; a live broadcast has no history, so a late joiner would miss
# the start of the stream (or the whole thing for a short clip).
#
# In --live mode the grader reads that stdout directly rather than a capture: it
# stamps each 188-byte read, which is the only way release timing survives at all
# (a file has none left in it). It stops itself after its own window, so the
# round-trip below still bounds the run.

# SCHEDULE carries the rate pcr-timing.py's schedule check grades against, and gates
# on it. The generated clip is muxed at $BITRATE, which is the rate the catalog records
# and the exporter pads to, on a constant-rate schedule from the first PCR:
# every interval is the bytes the rate implies, to one packet. A --source capture's
# rate is not known here, so for that the grader estimates it, says so, and only reports.
SCHEDULE=()
[[ -z "$SOURCE" ]] && SCHEDULE=(--mux-rate "$BITRATE" --schedule-pct-min 99)

# Both halves matter and `wait` can only report one, so record each. The
# exporter's own status is not incidental here: it decides whether the grader saw
# the whole window or graded a stream that ended under it.
# shellcheck disable=SC2329  # invoked indirectly via 'harness_spawn'
grade_live() {
    # `set -e` would abort on the pipeline's own failure, which is exactly the
    # status we are here to record.
    set +e
    timeout -k 3 $((DURATION + 20)) \
        "$MOQ" "${SUB_CONNECT[@]}" --connect-version "$VERSION" --broadcast "$BROADCAST" export ts ${EXPORT[@]+"${EXPORT[@]}"} 2>"$HARNESS_RUN/sub.log" |
        ts_drain python3 "$DIR/pcr-timing.py" --live --seconds "$DURATION" --release-pct-max 1 $STRICT \
            --report-json "$HARNESS_RUN/pcr-timing.json" \
            ${SCHEDULE[@]+"${SCHEDULE[@]}"} ${PASSTHRU[@]+"${PASSTHRU[@]}"} >"$HARNESS_RUN/timing.out" 2>&1
    printf '%s\n' "${PIPESTATUS[0]} ${PIPESTATUS[1]}" >"$HARNESS_RUN/timing.rc"
}

# shellcheck disable=SC2329  # invoked indirectly via 'harness_spawn'
capture() {
    timeout -k 3 $((DURATION + 20)) \
        "$MOQ" "${SUB_CONNECT[@]}" --connect-version "$VERSION" --broadcast "$BROADCAST" export ts ${EXPORT[@]+"${EXPORT[@]}"} >"$SUB_TS" 2>"$HARNESS_RUN/sub.log"
}

# shellcheck disable=SC2329  # invoked indirectly via 'harness_spawn'
capture_b() {
    # Joining late is the point. Two exporters started together can agree on a
    # cadence by having started together, which is the confound this arm exists to
    # remove: a leg that joins mid-broadcast has to derive its emission points from
    # the media, because it has no shared history to derive them from.
    sleep "$PAIR_JOIN"
    timeout -k 3 $((DURATION + 20)) \
        "$MOQ" "${SUB_CONNECT[@]}" --connect-version "$VERSION" --broadcast "$BROADCAST" export ts ${EXPORT[@]+"${EXPORT[@]}"} >"$SUB_B_TS" 2>"$HARNESS_RUN/sub-b.log"
}

if [[ -n "$LIVE" ]]; then
    echo "### grading subscriber output live (export ts | pcr-timing.py)"
    harness_spawn sub - grade_live
else
    echo "### capturing subscriber output (export ts)"
    harness_spawn sub - capture
fi
SUB_PID="$HARNESS_PID"
if [[ -n "$PAIR" ]]; then
    echo "### capturing a second subscriber, joining ${PAIR_JOIN}s late"
    harness_spawn sub-b - capture_b
    SUB_B_PID="$HARNESS_PID"
fi
sleep 1

# Pace on the source PCR (real media time), not a fixed bitrate: a synthetic clip
# compresses tiny, so bitrate pacing would rush the whole stream out in a blink.
# `--wait-min 5` is what makes that pacing fine-grained enough to measure against:
# at the default, `regulate` releases in ~50 ms chunks, which is jitter of its own
# on top of whatever the exporter does (measured: p95 40 ms at the default, 1 ms
# at 5). It is the publisher's granularity, not the exporter's, so the harness has
# to be well inside it or it grades tsp.
# Bounded like the subscriber: if the relay stalls after readiness, `moq import
# ts` could otherwise block `wait "$PUB_PID"` forever. The bound adds the 30 s
# default max-age a finished import stays up for after EOF. tsp/moq stderr lands
# in pub.log, which the empty-capture handler below dumps on failure.
echo "### publishing PCR-paced TS -> $BROADCAST"
# shellcheck disable=SC2329  # invoked indirectly via 'harness_spawn'
publish() {
    # shellcheck disable=SC2016  # $1..$5 are the child bash -c positionals, not ours.
    timeout -k 3 $((DURATION + 20 + 30)) bash -o pipefail -c '
        tsp -I file "$1" -P regulate --pcr-synchronous --wait-min 5 |
            "$2" --connect "$3" --connect-version "$5" --broadcast "$4" "${@:6}"
    ' _ "$SRC_TS" "$MOQ" "$URL" "$BROADCAST" "$VERSION" "${IMPORT[@]}"
}
harness_spawn pub "$HARNESS_RUN/pub.log" publish
PUB_PID="$HARNESS_PID"

# shellcheck disable=SC2329  # invoked from multiple failure paths below
dump_logs() {
    cat "$HARNESS_RUN/process-status.log" >&2 || true
    sed 's/^/  pub: /' "$HARNESS_RUN/pub.log" >&2 || true
    sed 's/^/  sub: /' "$HARNESS_RUN/sub.log" >&2 || true
}

if ! ts_wait "$PUB_PID" publisher; then
    dump_logs
    exit 1
fi

# SIGTERM, not the harness's SIGKILL: the shaper writes its counters and fails if the
# profile never acted, which would turn the impaired run into an unimpaired pass.
# shellcheck disable=SC2329  # invoked once the subscriber is done, below
stop_shaper() {
    [[ -n "$SHAPED" ]] || return 0
    kill -TERM -- -"$SHAPER_PID" 2>/dev/null || true
    ts_wait "$SHAPER_PID" shaper || {
        sed 's/^/  shaper: /' "$HARNESS_RUN/shaper.log" >&2 || true
        return 1
    }
    # The shaper accepts a silent profile while silence is still likely, and never requires a
    # rate limit to act; a lossy run needs loss, and a congested one a rate limit that held
    # datagrams back. A loss-based sender (MOQ_QUIC_CONGESTION_CONTROL=loss) grows until it
    # loses, and the queue's tail drop is the only loss here, so its run must overflow too.
    local counter overflow=""
    [[ -n "$RATE" && "${MOQ_QUIC_CONGESTION_CONTROL:-}" == loss ]] && overflow=overflowed
    for counter in ${LOSS:+lost} ${RATE:+throttled} $overflow; do
        if ! python3 -c 'import json, sys; sys.exit(json.load(open(sys.argv[1]))["down"][sys.argv[2]] == 0)' \
            "$HARNESS_RUN/shaper.json" "$counter"; then
            echo "error: moq-shaper ${counter} none of the relay's datagrams on a path of ${PATH_DESC}" >&2
            return 1
        fi
    done
    # A path narrower than the media congests the session, not the queue: the delay-based
    # default (BBRv3) keeps its inflight near two round trips' worth, so a deep queue rarely
    # overflows and the backlog lands at the relay, which sheds stale groups. So a congested
    # run is one whose export shed something; one that kept everything was never narrower.
    if [[ -n "$RATE" ]] && ! sed 's/\x1b\[[0-9;]*m//g' "$HARNESS_RUN/sub.log" | grep -E 'media skipped upstream|missed its deadline' >/dev/null; then
        echo "error: the export shed nothing, so ${RATE} b/s never narrowed the path below the media; lower --rate" >&2
        return 1
    fi
    # What the path dropped beside what that cost the output: QUIC retransmits the former,
    # and the exporter counts the units that still missed their deadline or never came.
    echo
    echo "### path (moq-shaper, seed $SEED)"
    tail -n 1 "$HARNESS_RUN/shaper.log" | sed 's/^shaper: /  /'
    sed 's/\x1b\[[0-9;]*m//g' "$HARNESS_RUN/sub.log" | grep -E 'TS (export|passthrough) release clock' | tail -n 1 |
        grep -oE 'dropped=[0-9]*( trimmed=[0-9]* gaps=[0-9]*)?' | sed 's/^/  export: /' || true
}

# ── live: the grader owns the verdict ───────────────────────────────────────
if [[ -n "$LIVE" ]]; then
    harness_wait "$SUB_PID" || true
    stop_shaper || exit 1
    if [[ ! -s "$HARNESS_RUN/timing.rc" ]]; then
        echo "error: the live grader never reported a status" >&2
        dump_logs
        exit 1
    fi
    read -r EXPORT_RC GRADE_RC <"$HARNESS_RUN/timing.rc"
    echo
    cat "$HARNESS_RUN/timing.out"
    # The grader drains after its measurement window, so no broken pipe or
    # timeout is an expected successful export.
    printf 'exporter: %s\ngrader: %s\n' "$EXPORT_RC" "$GRADE_RC" >>"$HARNESS_RUN/process-status.log"
    if [[ "$EXPORT_RC" -ne 0 ]]; then
        echo >&2
        echo "error: the exporter exited $EXPORT_RC" >&2
        dump_logs
        exit 1
    fi
    if [[ "$GRADE_RC" -ne 0 ]]; then
        echo >&2
        echo "error: PCR timing analysis failed (see round-trip logs below)" >&2
        dump_logs
        exit "$GRADE_RC"
    fi
    exit 0
fi

# Let the exporter drain to EOF within its existing timeout. Killing it after a
# sleep can both truncate a valid tail and conceal an earlier process failure.
EXPORTED=0
ts_wait "$SUB_PID" exporter || EXPORTED=$?
# The path's counters explain a failed export too, so they come first.
stop_shaper || exit 1
if [[ "$EXPORTED" -ne 0 ]]; then
    dump_logs
    exit 1
fi

if [[ ! -s "$SUB_TS" ]]; then
    echo "error: subscriber captured no data" >&2
    dump_logs
    exit 1
fi

# ── pair: the two captures are the measurement ──────────────────────────────
if [[ -n "$PAIR" ]]; then
    if ! ts_wait "$SUB_B_PID" exporter-b; then
        dump_logs
        cat "$HARNESS_RUN/sub-b.log" >&2
        exit 1
    fi
    if [[ ! -s "$SUB_B_TS" ]]; then
        echo "error: the second subscriber captured no data" >&2
        sed 's/^/  sub-b: /' "$HARNESS_RUN/sub-b.log" >&2 || true
        dump_logs
        exit 1
    fi
    echo "### captured $(wc -c <"$SUB_TS" | tr -d ' ') + $(wc -c <"$SUB_B_TS" | tr -d ' ') bytes -> comparing table anchors"
    echo
    if ! python3 "$DIR/table-anchor.py" "$SUB_TS" "$SUB_B_TS" --source "$SRC_TS" \
        --report-json "$HARNESS_RUN/table-anchor.json" $STRICT \
        ${PASSTHRU[@]+"${PASSTHRU[@]}"}; then
        echo >&2
        echo "error: table anchor analysis failed (see round-trip logs below)" >&2
        sed 's/^/  sub-b: /' "$HARNESS_RUN/sub-b.log" >&2 || true
        dump_logs
        exit 1
    fi
    exit 0
fi

if [[ -n "$CAPTURE_OUT" ]]; then
    cp "$SUB_TS" "$CAPTURE_OUT"
fi

echo "### captured $(wc -c <"$SUB_TS" | tr -d ' ') bytes -> analyzing"

# ── passthrough: the source is the oracle ───────────────────────────────────
# Every captured packet must be the source's at its exact position from the first one on, and
# compliance.py must grade the capture as it grades that interval of the source, whatever the
# source's own verdict. QUIC retransmits what --loss drops, so a gap fails here as it does under
# holes.py; passthrough.py says whether packets went missing or were rewritten. A path narrower
# than the multiplex (--rate) has to shed, so there gaps are reported rather than failed, but
# the capture must still end on the source's last packet.
if [[ -n "$PASSTHROUGH" ]]; then
    echo
    if ! python3 "$DIR/passthrough.py" "$SRC_TS" "$SUB_TS" ${RATE:+--loss-ok} --report-json "$HARNESS_RUN/passthrough.json"; then
        echo >&2
        echo "error: the capture is not the source's packets in place; the exporter reported:" >&2
        sed 's/\x1b\[[0-9;]*m//g' "$HARNESS_RUN/sub.log" |
            grep -E 'TS passthrough release clock|media skipped upstream|missed its deadline|group evicted' |
            tail -n 20 | sed 's/^/  sub: /' >&2 || true
        exit 1
    fi
    echo
    if [[ -z "$SOURCE$OPEN_GOP$HRD$HEADROOM$LEAK$FULL$MPTS$RATE" ]]; then
        python3 "$DIR/decode.py" "$SRC_TS" "$SUB_TS" --output "$HARNESS_RUN/decode" || exit 1
    else
        echo "decoded-content comparison: not covered for this profile"
    fi
    exit 0
fi

# The initial decoded-content profile is the generated closed-GOP H.264/AAC
# pair. Other arms retain their specific structural/model oracles until their
# acquisition and decoder-priming contracts are covered by fixtures. A path
# narrower than the media sheds groups, which only holes.py grades.
if [[ -z "$SOURCE$OPEN_GOP$HRD$HEADROOM$LEAK$FULL$KLV$RATE" ]]; then
    if ! python3 "$DIR/decode.py" "$SRC_TS" "$SUB_TS" --output "$HARNESS_RUN/decode"; then
        # The exporter counts what it dropped (late), trimmed (before its output began),
        # and lost upstream (gaps), and logs each loss, so the failure says which it was.
        echo >&2
        echo "error: the decoded capture differs from the source; the exporter reported:" >&2
        grep -E 'TS export release clock|media skipped upstream|missed its deadline|group evicted' \
            "$HARNESS_RUN/sub.log" | sed 's/^/  sub: /' >&2 || true
        exit 1
    fi
else
    echo "decoded-content comparison: not covered for this profile"
fi

# The other generated clips keep one frame step per stream, so media missing from the capture
# shows in its timestamps even where its content is not compared. The compliance checks below
# grade only the access units that arrived. Under --rate the path is narrower than the media,
# so the export has to shed: only whole groups, and never the last, which a stall or a
# latency that grew with the queue would cost.
if [[ -n "$HRD$HEADROOM$LEAK$FULL$KLV" || (-n "$RATE" && -z "$SOURCE") ]]; then
    echo
    if ! python3 "$DIR/holes.py" "$SUB_TS" "$SRC_TS" ${RATE:+--shed}; then
        echo >&2
        echo "error: the capture is missing media; the exporter reported:" >&2
        sed 's/\x1b\[[0-9;]*m//g' "$HARNESS_RUN/sub.log" |
            grep -E 'TS export release clock|media skipped upstream|missed its deadline|group evicted' |
            tail -n 20 | sed 's/^/  sub: /' >&2 || true
        exit 1
    fi
fi

# Which SI survived the round-trip. Informational: the exporter rebuilds SI from the
# catalog, so a PID the import path does not route simply is not there, and that is a
# statement about SI_PIDS rather than a malformed stream. compliance.py grades the stream
# an IRD receives; this says what it was carrying on the way in.
if [[ -n "$WITH_EIT" ]]; then
    echo
    echo "### SI round-trip (source -> capture)"
    count_pid() {
        tsp -I file "$1" -P count --pid "$2" --total -O drop 2>&1 |
            sed -n 's/.*counted \([0-9,]*\) packets.*/\1/p' | head -1
    }
    printf '  %-10s %-8s %12s %12s\n' TABLE PID SOURCE CAPTURE
    for spec in "NIT:0x0010" "SDT:0x0011" "EIT:0x0012" "TDT/TOT:0x0014"; do
        printf '  %-10s %-8s %12s %12s\n' "${spec%%:*}" "${spec##*:}" \
            "$(count_pid "$SRC_TS" "${spec##*:}")" "$(count_pid "$SUB_TS" "${spec##*:}")"
    done
fi

# Leading pictures are only decodable in continuous playback, which is the case this
# subscriber is in, so the round-trip owes every one of them, in decode order.
if [[ -n "$OPEN_GOP" ]]; then
    echo
    if ! python3 "$DIR/open-gop.py" "$SRC_TS" "$SUB_TS" $STRICT; then
        echo >&2
        echo "error: open-GOP analysis failed (see round-trip logs below)" >&2
        dump_logs
        exit 1
    fi
fi

# Each private PES is the source's, at the source's placement.
if [[ -n "$KLV" ]]; then
    echo
    if ! python3 "$DIR/klv.py" grade "$SRC_TS" "$SUB_TS" --report-json "$HARNESS_RUN/klv.json"; then
        echo >&2
        echo "error: KLV analysis failed (see round-trip logs below)" >&2
        dump_logs
        exit 1
    fi
fi

# The delay's lead over the source's hides packets the export spends beyond the source's
# until it runs out, so they are counted outright.
if [[ -n "$FULL" ]]; then
    echo
    if ! python3 "$DIR/overhead.py" "$SRC_TS" "$SUB_TS"; then
        echo >&2
        echo "error: the export spends more packets than its source on clocks and audio" >&2
        dump_logs
        exit 1
    fi
fi
echo
# Pass the source so duration-fidelity can pin the exported stream's rate. A tiny
# capture still parses, so the round-trip can fail here with a non-empty file;
# dump the logs and publisher status so the failure is diagnosable, not a mystery.
if ! analyze "$SUB_TS" "$SRC_TS"; then
    echo >&2
    echo "error: compliance analysis failed (see round-trip logs below)" >&2
    dump_logs
    exit 1
fi

# compliance.py grades rate in aggregate and over fixed windows, neither of which says
# whether the bytes between consecutive PCRs are the ones the mux rate implies, so the
# capture goes through pcr-timing.py as well. Its hard checks gate here as they do under
# --live, and so does pcr-schedule when the rate is known (SCHEDULE).
echo
if ! python3 "$DIR/pcr-timing.py" "$SUB_TS" --report-json "$HARNESS_RUN/pcr-timing.json" ${SCHEDULE[@]+"${SCHEDULE[@]}"} $STRICT; then
    echo >&2
    echo "error: PCR timing analysis failed (see round-trip logs below)" >&2
    dump_logs
    exit 1
fi

# pcr-schedule allows each PCR a packet of slack, but an IRD or a TR 101 290 probe holds
# every PCR to within 500 ns of its byte position at the mux rate (PCR_accuracy_error).
# TSDuck's pcrverify measures exactly that, including the first PCR interval.
if [[ ${#SCHEDULE[@]} -gt 0 ]]; then
    echo
    echo "### PCR accuracy (tsp -P pcrverify, +/-500 ns at ${BITRATE} b/s)"
    if ! python3 "$DIR/accuracy.py" "$SUB_TS" --bitrate "$BITRATE" --log "$HARNESS_RUN/pcrverify.log"; then
        echo >&2
        echo "error: PCRs stray from their byte position (see $HARNESS_RUN/pcrverify.log)" >&2
        dump_logs
        exit 1
    fi
fi
