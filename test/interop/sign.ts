/** Exchange signed groups with Rust: verify the ones Rust signed, and sign through the JS wrappers. */
import assert from "node:assert/strict";
import { Broadcast, Epoch, type Track as NetTrack, Path, Time } from "../../js/net/src/index.ts";
import * as Signed from "../../js/sign/src/broadcast.ts";
import { Failure } from "../../js/sign/src/failure.ts";
import { SecretKey } from "../../js/sign/src/key.ts";
import { Scope } from "../../js/sign/src/scope.ts";
import * as Track from "../../js/sign/src/track.ts";

/** One frame as it crosses the wire: its body, signature included, and its timestamp at the track's timescale. */
type Frame = { body: string; timestamp?: number | null };
/** One group of a track, timed when it has a timescale. */
type Group = { track: string; group: number; timescale?: number | null; frames: Frame[] };
/** A group for JS to sign: payloads, and timestamps in microseconds on a timed track. */
type Write = { track: string; timescale?: number | null; frames: { payload: string; micros?: number | null }[] };
type Input = { seed: string; path: string; epoch: string; rust: Group[]; js: Write[] };

function unhex(text: string): Uint8Array<ArrayBuffer> {
	return Uint8Array.from(text.match(/../g) ?? [], (byte) => Number.parseInt(byte, 16));
}

function hex(bytes: Uint8Array): string {
	return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

const input = JSON.parse(process.argv[2] ?? "") as Input;
const key = await SecretKey.fromSeed(unhex(input.seed));
const path = Path.from(input.path);
const epoch = Epoch.parse(input.epoch);

// Each Rust group through a verifying JS track consumer: its payloads, or the failure code.
const verified: (string[] | string)[] = [];
for (const { track, group, timescale, frames } of input.rust) {
	const scale = timescale == null ? undefined : Time.Timescale(timescale);
	const net = new Broadcast.Producer().createTrack(track, { timescale: scale });
	const consumer = Track.Consumer.make(net.subscribe(), key.public, new Scope(path, epoch, track));
	const writer = net.createGroup(group);
	for (const { body, timestamp } of frames) {
		const at = scale === undefined || timestamp == null ? undefined : new Time.Timestamp(timestamp, scale);
		writer.writeFrame({ payload: unhex(body), timestamp: at });
	}
	writer.finish();
	const received = await consumer.recvGroup();
	assert(received);
	try {
		const payloads: string[] = [];
		for (let frame = await received.readFrame(); frame; frame = await received.readFrame()) {
			payloads.push(hex(frame.payload));
		}
		verified.push(payloads);
	} catch (err) {
		if (!(err instanceof Failure)) throw err;
		verified.push(err.code);
	}
}

// Groups signed through the JS wrappers, tapped raw off the net broadcast.
const inner = new Broadcast.Producer();
const signed = new Signed.Producer(inner, key, path);
const producers = new Map<string, Track.Producer>();
const taps = new Map<string, NetTrack.Subscriber>();
const js: Group[] = [];
for (const { track, timescale, frames } of input.js) {
	const scale = timescale == null ? undefined : Time.Timescale(timescale);
	let producer = producers.get(track);
	if (!producer) {
		producer = signed.createTrack(track, { timescale: scale });
		producers.set(track, producer);
		taps.set(track, inner.consume().track(track).subscribe());
	}
	const group = producer.appendGroup();
	for (const { payload, micros } of frames) {
		await group.writeFrame(micros == null ? undefined : Time.Timestamp.fromMicros(micros), unhex(payload));
	}
	group.finish();
	const raw = await taps.get(track)?.recvGroup();
	assert(raw);
	const bodies: Frame[] = [];
	for (let frame = await raw.readFrame(); frame; frame = await raw.readFrame()) {
		bodies.push({ body: hex(frame.payload), timestamp: frame.timestamp?.value });
	}
	js.push({ track, group: raw.sequence, timescale, frames: bodies });
}

process.stdout.write(JSON.stringify({ public: key.public.toString(), verified, epoch: signed.epoch, js }));
