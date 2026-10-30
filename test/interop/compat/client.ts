import { readFileSync, writeFileSync } from "node:fs";
import { parseArgs } from "node:util";
import * as Auth from "@moq/auth";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import { Hex, Libav } from "@moq/hang/util";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import { install } from "@moq/web-transport";
import { connect } from "./transport";

// This one client runs against both the checkout's and the released packages, so it sticks to
// API both have: the group and frame primitives, the snapshot decoder, and the libav WebCodecs
// fallback, not the consumers above them, whose shapes follow the checkout.

// The released packages brand units under `Moq.Time`, the checkout's under @moq/clock: both are
// plain numbers at runtime, so cast rather than call either brand.
type FrameMicros = Parameters<typeof Container.Legacy.encodeFrame>[1];
type TimestampMicros = Parameters<typeof Moq.Time.Timestamp.fromMicros>[0];

/** A catalog section's first rendition, with its name. */
function first<T>(section: { renditions: Record<string, T> } | undefined): [string, T] | undefined {
	return Object.entries(section?.renditions ?? {})[0];
}

/** Read catalog updates until `pick` finds what it needs in one. */
async function catalog<T>(bc: Moq.Broadcast.Consumer, pick: (root: Catalog.Root) => T | undefined): Promise<T> {
	const track = bc.track("catalog.json").subscribe({ priority: Catalog.PRIORITY.catalog });
	const decoder = new Json.Snapshot.Decoder<Catalog.Root>({ schema: Catalog.RootSchema });
	for (;;) {
		const group = await track.recvGroup();
		if (!group) throw new Error("catalog ended without the renditions this reader needs");
		// Each group opens with a snapshot; the frames after it are deltas.
		let opened = false;
		try {
			for (let frame = await group.readFrame(); frame; frame = await group.readFrame()) {
				if (opened) decoder.delta(frame.payload);
				else decoder.snapshot(frame.payload);
				opened = true;
				const root = decoder.decode();
				const found = root && pick(root);
				if (found) return found;
			}
		} catch (err) {
			// The relay resets a group a newer snapshot supersedes; resync from the next.
			if (!(err instanceof Moq.Error.Stream)) throw err;
		}
	}
}

type Cmaf = typeof import("@moq/hang/cmaf");

/** A rendition's container format: the current CLI imports fMP4 as legacy frames, released ones as CMAF. */
async function openFormat(
	kind: "video" | "audio",
	config: Catalog.VideoConfig | Catalog.AudioConfig,
): Promise<Container.Format> {
	const container = config.container;
	if (container.kind === "legacy") return new Container.Legacy.Format(kind);
	if (container.kind !== "cmaf") throw new Error(`unsupported ${kind} container: ${JSON.stringify(container)}`);
	// The released package keeps CMAF under the container module; the checkout loads it on demand.
	const cmaf = (Container as { Cmaf?: Cmaf }).Cmaf ?? (await import("@moq/hang/cmaf"));
	return new cmaf.Format(cmaf.decodeInitSegment(Buffer.from(container.init, "base64")));
}

/** The byte count of the first non-empty video frame. */
async function video(bc: Moq.Broadcast.Consumer, [name, config]: [string, Catalog.VideoConfig]): Promise<number> {
	const format = await openFormat("video", config);
	const track = bc.track(name).subscribe({ priority: 0 });
	for (let group = await track.recvGroup(); group; group = await track.recvGroup()) {
		try {
			for (let frame = await group.readFrame(); frame; frame = await group.readFrame()) {
				const bytes = format
					.decode(frame.payload, frame.timestamp)
					.reduce((n, f) => n + f.payload.byteLength, 0);
				if (bytes > 0) return bytes;
			}
		} catch (err) {
			// The relay resets a group a newer one supersedes; move on to the next.
			if (!(err instanceof Moq.Error.Stream)) throw err;
		}
	}
	throw new Error("no frame data received");
}

// How far behind the newest audio a group may fall and still be read, in milliseconds.
const AUDIO_BUDGET = 5000;

/** The subscription field holding the latency budget, which the checkout renamed from `maxAge`. */
function budgetField(): "maxAge" | "maxDelay" {
	// The checkout refuses the old name, which a throwaway local track shows without a session.
	const broadcast = new Moq.Broadcast.Producer();
	try {
		broadcast
			.createTrack("probe")
			.subscribe({ maxAge: 0 } as Moq.Track.Subscription)
			.close();
		return "maxAge";
	} catch (err) {
		if (!(err instanceof TypeError)) throw err;
		return "maxDelay";
	} finally {
		broadcast.close();
	}
}

/** Interleaved f32 samples, copied in the decoder's own layout. */
function interleave(data: AudioData): Float32Array {
	// Never through a format conversion: the released libav fallback garbles every channel past the
	// first when it converts interleaved samples to planar, and this lane checks the wire, not that.
	const frames = data.numberOfFrames;
	const channels = data.numberOfChannels;
	const out = new Float32Array(frames * channels);
	if (data.format === "f32") {
		data.copyTo(out, { planeIndex: 0 });
	} else if (data.format === "f32-planar") {
		const plane = new Float32Array(frames);
		for (let c = 0; c < channels; c++) {
			data.copyTo(plane, { planeIndex: c });
			for (let i = 0; i < frames; i++) out[i * channels + c] = plane[i];
		}
	} else {
		throw new Error(`unexpected decoded sample format: ${data.format}`);
	}
	return out;
}

/**
 * Decode the first `ms` of the rendition to `{out}.{codec}.{txt,pcm}`, the layout tone.py reads, the codec
 * named by the catalog's up to its first `.`. Created exclusively, so two renditions of one codec fail.
 */
async function audio(
	bc: Moq.Broadcast.Consumer,
	[name, config]: [string, Catalog.AudioConfig],
	out: string,
	ms: number,
) {
	const format = await openFormat("audio", config);
	const want = (ms * config.sampleRate) / 1000;
	const log = [`${config.sampleRate} ${config.numberOfChannels}`];
	const pcm: Float32Array[] = [];
	let decoded = 0;
	let failed: Error | undefined;
	const { promise: done, resolve } = Promise.withResolvers<void>();
	const decoder = new AudioDecoder({
		output: (data) => {
			if (decoded < want) {
				log.push(`${data.timestamp} ${data.numberOfFrames}`);
				pcm.push(interleave(data));
				decoded += data.numberOfFrames;
				if (decoded >= want) resolve();
			}
			data.close();
		},
		error: (error) => {
			failed = error;
			resolve();
		},
	});
	// Opus in every container is raw packets; its description (an OpusHead) is not what WebCodecs wants.
	decoder.configure({
		codec: config.codec,
		sampleRate: config.sampleRate,
		numberOfChannels: config.numberOfChannels,
		description: config.codec === "opus" || !config.description ? undefined : Hex.toBytes(config.description),
	});

	// In sequence order, as a stateful decoder needs. An fMP4 fragment lands the tail of one audio
	// group and the head of the next in one burst, and the default budget of zero drops that tail
	// once the next group arrives, so allow a backlog; a group that never arrives still leaves a gap.
	const subscription = { priority: Catalog.PRIORITY.audio, [budgetField()]: AUDIO_BUDGET };
	const track = bc
		.track(name)
		.subscribe(subscription as Moq.Track.Subscription)
		.ordered();
	const feed = (async () => {
		for (let frame = await track.readFrame(); frame; frame = await track.readFrame()) {
			for (const { timestamp, payload } of format.decode(frame.payload, frame.timestamp)) {
				if (decoder.state === "closed") return;
				decoder.decode(new EncodedAudioChunk({ type: "key", data: payload, timestamp }));
			}
		}
		throw new Error("audio track ended early");
	})();
	try {
		await Promise.race([done, feed]);
	} finally {
		track.close();
		if (decoder.state !== "closed") decoder.close();
	}
	if (failed) throw failed;

	const prefix = `${out}.${config.codec.split(".")[0]}`;
	writeFileSync(`${prefix}.txt`, `${log.join("\n")}\n`, { flag: "wx" });
	writeFileSync(
		`${prefix}.pcm`,
		Buffer.concat(pcm.map((samples) => Buffer.from(samples.buffer, samples.byteOffset, samples.byteLength))),
		{ flag: "wx" },
	);
}

/** What a subscriber reads: a video frame and decoded audio, or the group live demand fills on a track. */
type Read = { kind: "media"; out: string; ms: number } | { kind: "group"; file: string; track?: string };

/** The released origin, which follows a path through `request`. */
type Released = { request(path: Moq.Path.Valid, options: { announced: true }): Moq.Origin.Requesting };

/** Print `received N bytes` once both media have arrived, or record the observed group. */
async function subscribe(url: URL, broadcast: string, read: Read) {
	install();
	// Before subscribing: compiling libav's wasm blocks the event loop long enough that a
	// subscription already open falls behind.
	if (read.kind === "media" && !(await Libav.polyfill())) throw new Error("no AudioDecoder");
	const origin = new Moq.Origin.Producer();
	const connection = await connect({ url, consume: origin, websocket: { enabled: false } });
	// Released packages follow with `request(path, { announced: true })`; current ones with `follow(path)`.
	const path = Moq.Path.from(broadcast);
	const requested =
		"follow" in origin ? origin.follow(path) : (origin as unknown as Released).request(path, { announced: true });
	try {
		let bc = requested.active.peek();
		while (!bc) {
			await requested.active.changed();
			bc = requested.active.peek();
		}

		if (read.kind === "group") {
			// Name a group that live demand is filling, so a FETCH of it never races eviction.
			const name = read.track ?? (await catalog(bc, (root) => first(root.video)?.[0]));
			const group = await bc.track(name).subscribe({ priority: 0 }).recvGroup();
			if (!group) throw new Error(`${name} ended before its first group`);
			writeFileSync(read.file, `${name}\n${group.sequence}\n`);
			return;
		}

		const renditions = await catalog(bc, (root) => {
			const [v, a] = [first(root.video), Object.entries(root.audio?.renditions ?? {})];
			return v && a.length > 0 ? { video: v, audio: a } : undefined;
		});
		const [bytes] = await Promise.all([
			video(bc, renditions.video),
			...renditions.audio.map((rendition) => audio(bc, rendition, read.out, read.ms)),
		]);
		// The harness judges success by this marker: the @moq/web-transport addon
		// can crash during exit teardown after a frame has arrived.
		console.error(
			`received ${bytes} bytes and ${read.ms}ms of ${renditions.audio.length} audio renditions from ${broadcast}`,
		);
	} finally {
		requested.close();
		// Wait for the close to reach the relay before exit, so it is not timed out.
		await connection.close();
		await connection.closed;
		origin.close();
	}
}

const [command, ...args] = process.argv.slice(2);
if (command === "sign") {
	const key = Auth.Key.parse(readFileSync(args[0], "utf8"));
	console.log(await Auth.Key.sign(key, { root: "compat", publish: ["video/**"], subscribe: ["**"] }));
} else if (command === "verify") {
	const key = Auth.Key.parse(readFileSync(args[0], "utf8"));
	const claims = await Auth.Key.verify(key, readFileSync(args[1], "utf8").trim());
	if (
		claims.root !== "compat" ||
		JSON.stringify(claims.publish) !== '["video/**"]' ||
		JSON.stringify(claims.subscribe) !== '["**"]'
	) {
		throw new Error(`token scope changed: ${JSON.stringify(claims)}`);
	}
} else if (command === "decode") {
	Catalog.RootSchema.parse(JSON.parse(readFileSync(args[0], "utf8")));
	const format = new Container.Legacy.Format("video");
	const decoded = format.decode(new Uint8Array(readFileSync(args[1])));
	if (
		decoded.length !== 1 ||
		decoded[0].timestamp !== 1000001 ||
		Buffer.from(decoded[0].payload).toString() !== "compat-frame"
	) {
		throw new Error("legacy container decoded the wrong timestamp or payload");
	}
} else if (command === "encode") {
	const catalog = Catalog.RootSchema.parse({
		video: { renditions: { video: { codec: "avc3.42001e", container: { kind: "legacy" } } } },
		audio: { renditions: {} },
	});
	writeFileSync(args[0], JSON.stringify(catalog));
	writeFileSync(
		args[1],
		Container.Legacy.encodeFrame(new TextEncoder().encode("compat-frame"), 1000001 as FrameMicros),
	);
} else if (command === "publish") {
	install();
	const origin = new Moq.Origin.Producer();
	const publication = origin.createBroadcast(Moq.Path.from(args[1]));
	// Released packages return the fill producer directly; current ones return its publication.
	const broadcast = ("broadcast" in publication ? publication.broadcast : publication) as Moq.Broadcast.Producer;
	const track = broadcast.createTrack("data", { timescale: Moq.Time.Timescale.MICRO });
	publication.announce();
	const connection = await connect({
		url: new URL(args[0]),
		publish: origin.consume(),
		websocket: { enabled: false },
	});
	// Stopped by the harness with SIGTERM: close, so the relay sees a close rather than timing it out.
	process.once("SIGTERM", () => void connection.close());
	console.log("published");
	// `demand()` replaced the producer's own `used` after @moq/net 0.4.2, then `wanted`
	// replaced `demand().used`, and this one client runs against each package.
	type Watch = { peek(): unknown; changed(): Promise<unknown> };
	const demand = ("demand" in track ? track.demand() : track) as unknown as { used?: Watch; wanted?: Watch };
	const watch = demand.wanted ?? demand.used;
	if (!watch) throw new Error("track has no demand");
	const wanted = () => (demand.wanted ? watch.peek() !== undefined : watch.peek() === true);
	while (!wanted()) await watch.changed();
	const group = track.appendGroup();
	group.writeFrame({
		payload: new TextEncoder().encode("compat-fetch"),
		timestamp: Moq.Time.Timestamp.fromMicros(1 as TimestampMicros),
	});
	group.finish();
	await connection.closed;
} else if (command === "subscribe") {
	// The flags of the interop subscriber this stands in for.
	const { values } = parseArgs({
		args,
		options: {
			url: { type: "string" },
			broadcast: { type: "string" },
			timeout: { type: "string", default: "20" },
			"audio-out": { type: "string" },
			"audio-ms": { type: "string" },
			"track-file": { type: "string" },
			track: { type: "string" },
		},
	});
	const timeout = Number.parseFloat(values.timeout ?? "") * 1000;
	const ms = Number.parseInt(values["audio-ms"] ?? "", 10);
	const file = values["track-file"];
	const out = values["audio-out"];
	const read: Read | undefined = file
		? { kind: "group", file, track: values.track }
		: out && ms > 0
			? { kind: "media", out, ms }
			: undefined;
	if (!values.url || !values.broadcast || !(timeout > 0) || !read) {
		throw new Error(
			"usage: subscribe --url U --broadcast B [--timeout S] (--audio-out PREFIX --audio-ms MS | --track-file F [--track T])",
		);
	}
	const timer = new Promise<never>((_, reject) =>
		setTimeout(() => reject(new Error("timed out waiting for data")), timeout),
	);
	try {
		await Promise.race([subscribe(new URL(values.url), values.broadcast, read), timer]);
		process.exit(0);
	} catch (err) {
		console.error(`error: ${err instanceof Error ? err.message : String(err)}`);
		process.exit(1);
	}
} else throw new Error(`unknown compatibility client command: ${command}`);
