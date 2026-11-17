// Captures the microphone twice, as a voice (gated) and kindless (ungated), encodes both through the
// real Audio.Encoder, and records every frame each writes. Exposes `globalThis.vad`.
import { Duration } from "@moq/clock";
import * as Moq from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import * as Audio from "../../../js/publish/src/audio/index.ts";
import { Order } from "../../../js/publish/src/order.ts";

type Written = { timestamp: number; payload: Uint8Array };

function rendition(written: Written[]) {
	const track = new Moq.Track.Producer("audio").accept({ timescale: Moq.Time.Timescale.MILLI });
	const appendGroup = track.appendGroup.bind(track);
	track.appendGroup = () => {
		const group = appendGroup();
		const writeFrame = group.writeFrame.bind(group);
		group.writeFrame = (frame) => {
			const [timestamp, payload] = Moq.Varint.decode(frame.payload);
			written.push({ timestamp, payload: payload.slice() });
			writeFrame(frame);
		};
		return group;
	};
	return {
		slot: new Signal(undefined),
		config: new Signal(undefined),
		stats: new Signal(undefined),
		track: new Signal<Moq.Track.Producer | undefined>(track),
		join: new Signal(undefined),
		close: () => track.close(),
	};
}

const params = new URLSearchParams(location.search);
const processing = params.get("processing") !== "off";
const constraints: MediaTrackConstraints = processing
	? {}
	: { echoCancellation: false, noiseSuppression: false, autoGainControl: false };

const stream = await navigator.mediaDevices.getUserMedia({ audio: constraints });
const track = stream.getAudioTracks()[0] as Audio.StreamTrack;

const runs: Record<string, { written: Written[]; encoder: Audio.Encoder }> = {};
// What the gated encoder's capture handed it, as the reference for what speech the gate dropped.
const raw: { timestamp: number; pcm: Float32Array }[] = [];
const effect = new Effect();
for (const kind of ["voice", "auto"] as const) {
	const written: Written[] = [];
	const capture = new Audio.Capture({ source: { track: kind === "voice" ? track : track.clone(), kind } });
	const encoder = new Audio.Encoder(kind, {
		broadcast: {
			audio: () => rendition(written),
			order: new Order(),
			in: { maxAge: new Signal(undefined), onDemand: new Signal("off") },
		} as never,
		capture,
	});
	runs[kind] = { written, encoder };
	if (kind === "voice") {
		effect.run((e) => {
			const fanout = e.get(capture.out.frames);
			if (!fanout) return;
			const reader = fanout.subscribe(e).getReader();
			e.spawn(async () => {
				for (;;) {
					const next = await reader.read();
					if (!next.value) break;
					const ch = next.value.channels;
					const mono = new Float32Array(ch[0].length);
					for (const c of ch) for (let i = 0; i < mono.length; i++) mono[i] += c[i] / ch.length;
					raw.push({ timestamp: next.value.timestamp.asMicros(), pcm: mono });
				}
			});
		});
	}
}

// Decode a run onto one timeline at 48 kHz from `origin` µs, resetting at each declared end as a
// player does.
async function decode(written: Written[], origin: number, length: number): Promise<Float32Array> {
	const out = new Float32Array(length);
	const decoder = new AudioDecoder({
		output: (data) => {
			const at = Math.round(((data.timestamp - origin) / 1_000_000) * 48_000);
			const pcm = new Float32Array(data.numberOfFrames);
			data.copyTo(pcm, { planeIndex: 0, format: "f32-planar" });
			for (let i = 0; i < pcm.length; i++) if (at + i >= 0 && at + i < length) out[at + i] = pcm[i];
			data.close();
		},
		error: (e) => console.error(e),
	});
	const configure = () => decoder.configure({ codec: "opus", sampleRate: 48_000, numberOfChannels: 1 });
	configure();
	for (const { timestamp, payload } of written) {
		if (payload.byteLength === 0) {
			await decoder.flush();
			decoder.reset();
			configure();
			continue;
		}
		decoder.decode(new EncodedAudioChunk({ type: "key", timestamp, data: payload }));
	}
	await decoder.flush();
	decoder.close();
	return out;
}

function base64(bytes: Uint8Array): string {
	let binary = "";
	for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
	return btoa(binary);
}

globalThis.vad = {
	settings: () => track.getSettings(),
	counts: () => Object.fromEntries(Object.entries(runs).map(([k, r]) => [k, r.written.length])),
	async result(seconds: number) {
		const first = Math.min(...Object.values(runs).map((r) => r.written.find((w) => w.payload.byteLength)?.timestamp ?? Infinity));
		const length = seconds * 48_000;
		const out: Record<string, unknown> = { origin: first, settings: track.getSettings(), userAgent: navigator.userAgent };
		const input = new Float32Array(length);
		for (const { timestamp, pcm } of raw) {
			const at = Math.round(((timestamp - first) / 1_000_000) * 48_000);
			for (let i = 0; i < pcm.length; i++) if (at + i >= 0 && at + i < length) input[at + i] = pcm[i];
		}
		out.input = { pcm: base64(new Uint8Array(input.buffer)) };
		for (const [kind, run] of Object.entries(runs)) {
			const pcm = await decode(run.written, first, length);
			out[kind] = {
				frames: run.written.map((w) => [w.timestamp - first, w.payload.byteLength]),
				pcm: base64(new Uint8Array(pcm.buffer)),
			};
		}
		return out;
	},
};
void Duration;
