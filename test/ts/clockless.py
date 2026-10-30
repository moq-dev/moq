#!/usr/bin/env python3
"""Generate and grade programs clocked by audio or a dedicated PCR-only PID."""

import argparse
from collections import deque
from pathlib import Path

import compliance
import klv

DATA = 0x100
AUDIO = 0x101
CLOCK = 0x1FF
PMT = 0x20
HZ = 27_000_000
CUE = bytes.fromhex("fc301b00000000000000fff00a0500002bb47fdf000100000000ad25e839")


def section(table, body):
    length = len(body) + 9
    value = bytes([table, 0xB0 | length >> 8, length & 255, 0, 1, 0xC1, 0, 0]) + body
    return value + compliance.crc32_mpeg(value).to_bytes(4, "big")


def clock_packet(route, ticks, counter):
    base, ext = divmod(ticks % klv.PCR_WRAP, 300)
    stamp = bytes(
        [base >> 25, base >> 17 & 255, base >> 9 & 255, base >> 1 & 255, (base & 1) << 7 | 0x7E | ext >> 8, ext & 255]
    )
    return bytes([0x47, route >> 8, route & 255, 0x20 | counter, 183, 0x10]) + stamp + b"\xff" * 176


def generate(kind, duration, bitrate, audio=b""):
    if duration <= 0 or bitrate <= 0:
        raise ValueError("a positive duration and multiplex rate are required")
    with_audio = kind == "audio-scte35"
    if with_audio and (not audio or len(audio) % 384):
        raise ValueError("expected complete 128 kb/s, 48 kHz MP2 frames")
    clock = AUDIO if with_audio else CLOCK
    info = b"\x05\x04CUEI" if kind != "pes" else b""
    streams = bytes([0x06 if kind == "pes" else 0x86, 0xE0 | DATA >> 8, DATA & 255, 0xF0, 0])
    if with_audio:
        streams += bytes([0x03, 0xE0 | AUDIO >> 8, AUDIO & 255, 0xF0, 0])
    pat = section(0, bytes([0, 1, 0xE0 | PMT >> 8, PMT & 255]))
    pmt = section(2, bytes([0xE0 | clock >> 8, clock & 255, 0xF0, len(info)]) + info + streams)
    counters = {}
    queue = deque()
    out = bytearray()
    next_pcr = next_tables = next_data = next_audio = 0
    audio_index = data_index = 0

    def enqueue(route, value):
        packets = klv.split(route, value, counters.get(route, 0))
        counters[route] = (counters.get(route, 0) + len(packets)) & 15
        queue.extend(packets)

    count = int(duration * bitrate / (188 * 8))
    for index in range(count):
        elapsed = index * 188 * 8 * HZ // bitrate
        if elapsed >= next_tables:
            enqueue(0, b"\0" + pat)
            enqueue(PMT, b"\0" + pmt)
            next_tables += HZ // 2
        if elapsed >= next_data:
            if kind == "pes":
                enqueue(DATA, klv.async_klv(data_index))
            else:
                cue = CUE[:-4]
                cue = cue[:14] + (data_index + 1).to_bytes(4, "big") + cue[18:]
                enqueue(DATA, b"\0" + cue + compliance.crc32_mpeg(cue).to_bytes(4, "big"))
            next_data += HZ // 2
            data_index += 1
        if with_audio and elapsed >= next_audio and audio_index * 384 < len(audio):
            frame = audio[audio_index * 384 : (audio_index + 1) * 384]
            pts = 90_000 + audio_index * 2_160 + 9_000
            enqueue(AUDIO, klv.pes_with(0xC0, 0x80, klv.clock_field(2, pts), frame))
            audio_index += 1
            next_audio += HZ * 1152 // 48000
        # Put initial tables before the first PCR, and thereafter put its reference byte
        # exactly where the CBR clock says it is, including a PCR on an audio PID.
        if index >= 2 and elapsed >= next_pcr:
            ticks = HZ + (index * 188 + 10) * 8 * HZ // bitrate
            packet = clock_packet(clock, ticks, (counters.get(clock, 1) - 1) & 15)
            next_pcr += HZ // 50
        else:
            packet = queue.popleft() if queue else bytes([0x47, 0x1F, 0xFF, 0x10]) + b"\xff" * 184
        out.extend(packet)
    if queue:
        raise ValueError("the generated program ended with queued packets")
    return bytes(out)


def units(data, pes):
    if pes:
        return [(unit.at, unit.data) for unit in klv.private_pes(data, {DATA})[DATA]]
    return [
        (index * 188 + 188 - len(klv.payload(packet)) + 1 + klv.payload(packet)[0], value)
        for index, packet in enumerate(klv.packets(data))
        if klv.pid(packet) == DATA and (value := klv.single_section(packet)) is not None
    ]


def grade(source, capture, kind):
    before, after = klv.program(source), klv.program(capture)
    if before.streams != after.streams:
        raise ValueError("the PMT changed the program's elementary streams")
    if kind == "audio-scte35":
        if after.pcr_pid != AUDIO:
            raise ValueError("the PMT must put PCR on the audio PID")
    elif after.pcr_pid in after.streams:
        raise ValueError("a data-only program must use a dedicated PCR PID")
    clocks = []
    for packet in klv.packets(capture):
        if klv.pid(packet) == after.pcr_pid:
            if kind != "audio-scte35" and packet[3] & 0x30 != 0x20:
                raise ValueError("a dedicated PCR PID carried a payload")
            if (ticks := klv.pcr(packet)) is not None:
                clocks.append(ticks)
    if len(clocks) < 2 or any((b - a) % klv.PCR_WRAP > HZ * 40 // 1000 for a, b in zip(clocks, clocks[1:])):
        raise ValueError("PCR samples are missing or more than 40 ms apart")
    expected = units(source, kind == "pes")
    actual = units(capture, kind == "pes")
    if not actual:
        raise ValueError("the captured program has no data units")
    first = next((i for i, (_, payload) in enumerate(expected) if payload == actual[0][1]), None)
    if first is None or [payload for _, payload in expected[first:]] != [payload for _, payload in actual]:
        raise ValueError("data payloads are not a byte-exact source suffix through EOF")
    input_clock, _ = klv.clock(source, before.pcr_pid)
    output_clock, _ = klv.clock(capture, after.pcr_pid)
    for (src_at, _), (dst_at, _) in zip(expected[first:], actual):
        if output_clock(dst_at) < input_clock(src_at):
            raise ValueError("a data unit went out before its source placement")
    print(f"clockless {kind}: {len(actual)} exact data units, PCR PID {after.pcr_pid:#x}, {len(clocks)} clock samples")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["generate", "grade"])
    parser.add_argument("kind", choices=["audio-scte35", "sections", "pes"])
    parser.add_argument("source", type=Path)
    parser.add_argument("capture", nargs="?", type=Path)
    parser.add_argument("--duration", type=float, default=20)
    parser.add_argument("--bitrate", type=int, default=10_000_000)
    parser.add_argument("--audio", type=Path)
    args = parser.parse_args()
    if args.command == "generate":
        args.source.write_bytes(
            generate(args.kind, args.duration, args.bitrate, args.audio.read_bytes() if args.audio else b"")
        )
    elif args.capture is None:
        parser.error("grade requires a capture")
    else:
        grade(args.source.read_bytes(), args.capture.read_bytes(), args.kind)


if __name__ == "__main__":
    main()
