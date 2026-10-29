/**
 * Native-JS (non-browser) interop subscriber: run the workspace `@moq/net` +
 * `@moq/hang` under a runtime with no native WebTransport, via moq's own
 * `@moq/web-transport` polyfill (a prebuilt NAPI QUIC/HTTP3 addon, the one piece
 * that comes from npm rather than this checkout). Runs under both node and bun.
 * Connect, wait for a .hang catalog with video and audio, read video through the
 * same container consumer the browser player uses until a non-empty frame arrives,
 * and decode the first --audio-ms of every audio rendition to --audio-out.<codec> for
 * tone.py to judge.
 * Neither runtime has WebCodecs, so audio decodes through `@moq/hang`'s libav
 * fallback, as a player under them would. Exits 0 once both are done (1 on
 * timeout). Subscribe-only: publishing media needs a WebCodecs encoder a native
 * JS runtime lacks.
 *
 *     node --import tsx subscribe.ts subscribe --url http://127.0.0.1:4443 --broadcast b.hang --timeout 20 \
 *         --audio-out /tmp/audio --audio-ms 600
 *
 * `--version` offers only that version. Any grant the relay sends over AUTH is printed as an
 * `auth granted` line.
 *
 * @module
 */

import { writeFileSync } from "node:fs";
import { createInterface } from "node:readline/promises";
import { parseArgs } from "node:util";
import { Milli } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Cmaf from "@moq/hang/cmaf";
import * as Container from "@moq/hang/container";
import { Hex, Libav } from "@moq/hang/util";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import { install } from "@moq/web-transport";
import { readFinish } from "./finish";

// globalThis.WebTransport = the polyfill (no-op if a native one already exists).
// @moq/net's connect() reads globalThis.WebTransport at call time, so this just
// has to run before run() below.
install();

// How stale a video group may get before it is skipped. A relay drops a
// superseded group (RESET_STREAM Old) rather than finish sending it, e.g. a
// cached group a fresh one lands right behind, so a subscriber has to move on
// to the next group instead of failing.
const MAX_DELAY = Milli(1000);

// Audio is checked for gaps, which a reader skipping behind live would cause itself: the matrix
// starts every subscriber at once, and a runtime starved by the others can fall a second behind.
// That is this reader's speed, not the delivery under test, so it gets a wider budget. A track's
// max-age caps it, so interop.sh's Rust publisher keeps its media at least this long.
const AUDIO_MAX_DELAY = Milli(5000);

const { positionals, values } = parseArgs({
	allowPositionals: true,
	options: {
		url: { type: "string" },
		broadcast: { type: "string" },
		timeout: { type: "string", default: "20" },
		version: { type: "string" },
		finish: { type: "boolean", default: false },
		frames: { type: "string", default: "10" },
		"audio-out": { type: "string" },
		"audio-ms": { type: "string" },
	},
});

const role = positionals[0];
const url = values.url;
const broadcast = values.broadcast;
const frames = Number(values.frames);
if (!Number.isSafeInteger(frames) || frames <= 0) throw new Error("frames must be a positive integer");
const audioOut = values["audio-out"];
const audioMs = Number.parseInt(values["audio-ms"] ?? "", 10);
const timeoutMs = Number.parseFloat(values.timeout ?? "20") * 1000;
if (
	role !== "subscribe" ||
	!url ||
	!broadcast ||
	(!values.finish && (!audioOut || !(audioMs > 0))) ||
	!Number.isFinite(timeoutMs) ||
	timeoutMs <= 0
) {
	console.error(
		"usage: subscribe.ts subscribe --url U --broadcast B (--audio-out PREFIX --audio-ms MS | --finish [--frames N]) [--timeout S>0] [--version V]",
	);
	process.exit(2);
}

// The .hang catalog lives on the "catalog.json" track. It's a @moq/json snapshot+delta
// value, reconstructed by Json.Snapshot.Consumer. A lazy publisher may announce a
// rendition in a later update, so keep reading until one has both video and audio.
async function catalog(
	bc: Moq.Broadcast.Consumer,
): Promise<{ video: [string, Catalog.VideoConfig]; audio: [string, Catalog.AudioConfig][] }> {
	const track = bc.track("catalog.json").subscribe({ priority: Catalog.PRIORITY.catalog });
	const catalog = new Json.Snapshot.Consumer<Catalog.Root>({ track, schema: Catalog.RootSchema });
	for (;;) {
		const root = (await catalog.latest())?.value;
		if (!root) throw new Error("catalog ended without a video and an audio track");
		const video = Object.entries(root.video?.renditions ?? {})[0];
		const audio = Object.entries(root.audio?.renditions ?? {});
		if (video && audio.length) return { video, audio };
	}
}

function containerFormat(config: Catalog.VideoConfig | Catalog.AudioConfig, kind: "video" | "audio"): Container.Format {
	switch (config.container.kind) {
		case "legacy":
			return new Container.Legacy.Format(config);
		case "loc":
			return new Container.Loc.Format(kind);
		case "cmaf":
			return new Cmaf.Format(Cmaf.decodeInitSegment(Buffer.from(config.container.init, "base64")));
		default:
			throw new Error(`unsupported ${kind} container: ${JSON.stringify(config.container)}`);
	}
}

async function video(bc: Moq.Broadcast.Consumer, name: string, config: Catalog.VideoConfig): Promise<number> {
	const sub = bc.track(name).subscribe({ priority: 0, maxDelay: MAX_DELAY });
	const consumer = new Container.Consumer(sub, { format: containerFormat(config, "video"), maxDelay: MAX_DELAY });
	try {
		for (;;) {
			const next = await consumer.next();
			if (!next) break;
			const bytes = next.kind === "frame" ? next.frame.payload.byteLength : 0;
			if (bytes > 0) return bytes;
		}
	} finally {
		consumer.close();
	}
	throw new Error("no frame data received");
}

/**
 * Decode the first {@link audioMs} of the rendition to `{audioOut}.{codec}.{txt,pcm}`, the layout tone.py
 * reads. The codec is the catalog's up to its first `.` (`opus`, `mp4a`). The files are created exclusively,
 * so two renditions of one codec fail rather than overwrite each other.
 */
async function audio(bc: Moq.Broadcast.Consumer, name: string, config: Catalog.AudioConfig): Promise<void> {
	const channels = config.numberOfChannels;
	const want = (audioMs * config.sampleRate) / 1000;
	const log = [`${config.sampleRate} ${channels}`];
	const pcm: Float32Array[] = [];
	let decoded = 0;
	let failed: Error | undefined;

	const { promise: done, resolve } = Promise.withResolvers<void>();
	const decoder = new AudioDecoder({
		output: (data) => {
			if (decoded < want) {
				// Every AudioData converts to f32-planar; interleave it the way the other clients write.
				const frames = data.numberOfFrames;
				const planes = Array.from({ length: data.numberOfChannels }, (_, plane) => {
					const samples = new Float32Array(frames);
					data.copyTo(samples, { planeIndex: plane, format: "f32-planar" });
					return samples;
				});
				const interleaved = new Float32Array(frames * planes.length);
				for (let i = 0; i < frames; i++) {
					for (const [c, plane] of planes.entries()) interleaved[i * planes.length + c] = plane[i];
				}
				log.push(`${data.timestamp} ${frames}`);
				pcm.push(interleaved);
				decoded += frames;
				if (decoded >= want) resolve();
			}
			data.close();
		},
		error: (error) => {
			failed = error;
			resolve();
		},
	});
	// Opus in every container is raw packets; its description (an OpusHead or dOps) is not
	// what WebCodecs wants, which is why the browser player leaves it out too.
	decoder.configure({
		codec: config.codec,
		sampleRate: config.sampleRate,
		numberOfChannels: channels,
		description: config.codec === "opus" || !config.description ? undefined : Hex.toBytes(config.description),
	});

	const sub = bc.track(name).subscribe({ priority: Catalog.PRIORITY.audio, maxDelay: AUDIO_MAX_DELAY });
	const consumer = new Container.Consumer(sub, {
		format: containerFormat(config, "audio"),
		maxDelay: AUDIO_MAX_DELAY,
	});
	const feed = (async () => {
		// The consumer also hands back the backlog the relay served behind the live edge, older
		// groups after newer ones. A player places those by timestamp; a stateful Opus decoder can
		// only move forward, so skip them, as video does.
		let last = Number.NEGATIVE_INFINITY;
		for (;;) {
			const next = await consumer.next();
			if (!next) throw new Error("audio track ended early");
			if (next.kind !== "frame" || next.frame.timestamp <= last) continue;
			last = next.frame.timestamp;
			if (decoder.state === "closed") return;
			decoder.decode(
				new EncodedAudioChunk({ type: "key", data: next.frame.payload, timestamp: next.frame.timestamp }),
			);
		}
	})();
	try {
		await Promise.race([done, feed]);
	} finally {
		consumer.close();
		if (decoder.state !== "closed") decoder.close();
	}
	if (failed) throw failed;

	const out = `${audioOut}.${config.codec.split(".")[0]}`;
	writeFileSync(`${out}.txt`, `${log.join("\n")}\n`, { flag: "wx" });
	writeFileSync(
		`${out}.pcm`,
		Buffer.concat(pcm.map((samples) => Buffer.from(samples.buffer, samples.byteOffset, samples.byteLength))),
		{ flag: "wx" },
	);
}

async function run(): Promise<void> {
	// Before subscribing: compiling libav's wasm blocks the event loop long enough that a
	// subscription already open falls a group behind and skips it.
	if (!(await Libav.polyfill())) throw new Error("no AudioDecoder");

	const origin = new Moq.Origin.Producer();
	const connection = await Moq.Connection.connect({
		url: new URL(url as string),
		consume: origin,
		...(values.version ? { versions: [values.version] } : {}),
		websocket: { enabled: false },
	});
	// The grant the relay sent, in the Rust client's `auth granted` shape, so the harness can
	// check it against the token this cell minted.
	const printGrant = (grant: Moq.Auth.Grant | undefined) => {
		if (!grant) return;
		const publish = JSON.stringify(grant.publish);
		const subscribe = JSON.stringify(grant.subscribe);
		console.error(`auth granted publish=${publish} subscribe=${subscribe}`);
	};
	printGrant(connection.auth.grant.peek());
	const unwatch = connection.auth.grant.subscribe(printGrant);
	let requested: Moq.Origin.Requesting | undefined;
	try {
		if (values.finish) {
			console.error("finish reader ready");
			const input = createInterface({ input: process.stdin });
			try {
				if ((await input.question("")) !== "read") throw new Error("expected finish read release");
			} finally {
				input.close();
			}
			console.error(await readFinish(origin, broadcast as string, frames, Milli(timeoutMs)));
			return;
		}
		const path = Moq.Path.from(broadcast as string);
		requested = origin.follow(path);
		let bc = requested.active.peek();
		while (!bc) {
			await requested.active.changed();
			bc = requested.active.peek();
		}

		const renditions = await catalog(bc);
		const [bytes] = await Promise.all([
			video(bc, ...renditions.video),
			...renditions.audio.map((rendition) => audio(bc, ...rendition)),
		]);
		// The harness judges success by this marker, not the exit code: the
		// @moq/web-transport NAPI addon can segfault during the runtime's exit
		// teardown after a frame has arrived (an upstream bug, seen under bun),
		// which would turn a real success into a signal exit.
		console.error(
			`received ${bytes} bytes and ${audioMs}ms of ${renditions.audio.length} audio renditions from ${broadcast}`,
		);
	} finally {
		unwatch();
		requested?.close();
		// Wait for the transport to report the close too, so it reaches the relay before
		// process.exit; otherwise the relay times the connection out.
		await connection.close();
		await connection.closed;
		origin.close();
	}
}

const timeout = new Promise<never>((_, reject) =>
	setTimeout(() => reject(new Error("timed out waiting for data")), timeoutMs * (values.finish ? 2 : 1)),
);

try {
	await Promise.race([run(), timeout]);
	process.exit(0);
} catch (err) {
	console.error(`error: ${err instanceof Error ? err.message : String(err)}`);
	process.exit(1);
}
