/** Publish a synthetic picture with @moq/publish and watch it with @moq/watch through the relay. */
import { Milli } from "@moq/clock";
import * as Moq from "@moq/net";
import * as Publish from "@moq/publish";
import * as Watch from "@moq/watch";
import { longTasks } from "./observe.ts";

/** One media workload. Everything that changes the decode or render path is fixed here. */
export interface Media {
	url: string;
	path: string;
	width: number;
	height: number;
	fps: number;
	codec: string;
	bitrate: number;
	keyframe_ms: number;
	/** Read each presented frame's id back, for frame delay. Kept out of the timed pass. */
	probe: boolean;
}

// The frame id is painted as bits in a strip of blocks along the top, with a check nibble so a
// blended or half-decoded strip is discarded instead of misread.
const ID_BITS = 12;
const CHECK_BITS = 4;
const BLOCKS = ID_BITS + CHECK_BITS;
const STRIP = 32;

function checksum(id: number): number {
	return (id * 7 + 3) & ((1 << CHECK_BITS) - 1);
}

// Wall-clock milliseconds, comparable between the publisher and watcher pages on one host.
function now(): number {
	return performance.timeOrigin + performance.now();
}

const cleanup: (() => void)[] = [];

// ── publisher ──────────────────────────────────────────────────────────────

const painted = new Map<number, number>();
let encoder: Publish.Video.Encoder | undefined;

async function publish(config: Media): Promise<void> {
	const canvas = document.createElement("canvas");
	canvas.width = config.width;
	canvas.height = config.height;
	document.body.append(canvas);
	const context = canvas.getContext("2d");
	if (!context) throw new Error("no 2d context");
	// Generated once, outside every timed interval; scrolled each frame so the encoder sees motion.
	const background = await scenery(config.width, config.height);
	const stream = canvas.captureStream(0);
	const track = stream.getVideoTracks()[0] as CanvasCaptureMediaStreamTrack;

	const connection = new Moq.Connection({ url: new URL(config.url), websocket: { enabled: false } });
	const capture = new Publish.Video.Capture({ source: track as unknown as Publish.Video.StreamTrack });
	const broadcast = new Publish.Broadcast({
		origin: connection.origin,
		enabled: true,
		name: Moq.Path.from(config.path),
		display: capture.out.display,
	});
	encoder = new Publish.Video.Encoder("video", {
		broadcast,
		capture,
		enabled: true,
		config: {
			codec: config.codec,
			frameRate: config.fps,
			maxPixels: config.width * config.height,
			keyframeInterval: Milli(config.keyframe_ms),
			maxBitrate: config.bitrate,
		},
	});

	const start = performance.now();
	let frame = 0;
	const tick = () => {
		const id = frame % (1 << ID_BITS);
		const offset = (frame * 4) % config.width;
		context.drawImage(background, -offset, 0);
		context.drawImage(background, config.width - offset, 0);
		const block = config.width / BLOCKS;
		const bits = (id << CHECK_BITS) | checksum(id);
		for (let bit = 0; bit < BLOCKS; bit++) {
			context.fillStyle = (bits >> (BLOCKS - 1 - bit)) & 1 ? "#fff" : "#000";
			context.fillRect(bit * block, 0, block, STRIP);
		}
		track.requestFrame();
		painted.set(id, now());
		frame++;
		// Scheduled against the start, so timer jitter does not accumulate into a lower frame rate.
		timer = setTimeout(tick, Math.max(0, start + (frame * 1000) / config.fps - performance.now()));
	};
	let timer = setTimeout(tick, 0);
	cleanup.push(() => {
		clearTimeout(timer);
		encoder?.close();
		broadcast.close();
		capture.close();
		track.stop();
		connection.close();
	});
}

async function scenery(width: number, height: number): Promise<ImageBitmap> {
	const canvas = new OffscreenCanvas(width, height);
	const context = canvas.getContext("2d");
	if (!context) throw new Error("no offscreen 2d context");
	const gradient = context.createLinearGradient(0, 0, width, height);
	gradient.addColorStop(0, "#204060");
	gradient.addColorStop(0.5, "#c08040");
	gradient.addColorStop(1, "#40a080");
	context.fillStyle = gradient;
	context.fillRect(0, 0, width, height);
	// A fixed-seed scatter, so every run encodes the same picture.
	let seed = 1;
	const random = () => {
		seed = (Math.imul(seed, 1103515245) + 12345) >>> 0;
		return seed / 2 ** 32;
	};
	for (let i = 0; i < 600; i++) {
		context.fillStyle = `hsl(${Math.floor(random() * 360)} 60% ${30 + Math.floor(random() * 40)}%)`;
		context.fillRect(random() * width, STRIP + random() * (height - STRIP), 8 + random() * 48, 8 + random() * 48);
	}
	return canvas.transferToImageBitmap();
}

/** Encoder output so far, read at the window's edges. */
function published() {
	const stats = encoder?.out.stats.peek();
	return {
		codec: encoder?.out.catalog.peek()?.codec ?? "unavailable",
		frames: stats?.frames ?? 0,
		bytes: stats?.bytes ?? 0,
		keyframes: stats?.keyframes ?? 0,
	};
}

/** Paint times by frame id, for joining against the watcher's presentation times. */
function paints(): [number, number][] {
	return [...painted];
}

// ── watcher ────────────────────────────────────────────────────────────────

// Counters over the watcher's VideoDecoder. Installed before the player creates one.
const decoder = { submitted: 0, decoded: 0, queue_sum: 0, queue_max: 0 };
const Native = globalThis.VideoDecoder;
if (Native) {
	globalThis.VideoDecoder = class extends Native {
		constructor(init: VideoDecoderInit) {
			super({
				...init,
				output: (frame) => {
					decoder.decoded++;
					init.output(frame);
				},
			});
		}
		override decode(chunk: EncodedVideoChunk): void {
			super.decode(chunk);
			decoder.submitted++;
			decoder.queue_sum += this.decodeQueueSize;
			decoder.queue_max = Math.max(decoder.queue_max, this.decodeQueueSize);
		}
	};
}

let player: Watch.Player | undefined;
let presented = 0;
// Frame id to [wall-clock ms, page-clock ms] at first presentation.
const presentedAt = new Map<number, [number, number]>();
const heap: [number, number][] = [];

async function watch(config: Media): Promise<{ first_frame_ms: number }> {
	const start = performance.now();
	const canvas = document.createElement("canvas");
	document.body.append(canvas);
	const connection = new Moq.Connection({ url: new URL(config.url), websocket: { enabled: false } });
	player = new Watch.Player({
		origin: connection.origin,
		probe: connection.probe,
		name: Moq.Path.from(config.path),
		video: { canvas, visible: "always", backend: "2d" },
		audio: { muted: true },
	});
	const sampler = setInterval(() => {
		const memory = (performance as Performance & { memory?: { usedJSHeapSize: number } }).memory;
		if (memory) heap.push([performance.now(), memory.usedJSHeapSize]);
	}, 250);
	const first = new Promise<number>((resolve) => {
		let last: number | undefined;
		const stop = player?.renderer.out.frame.subscribe((frame) => {
			// A resize or catalog change repaints the same frame; count each frame once.
			if (!frame || frame.timestamp === last) return;
			last = frame.timestamp;
			presented++;
			if (presented === 1) resolve(performance.now() - start);
			if (config.probe) probe(frame);
		});
		cleanup.push(() => stop?.());
	});
	cleanup.push(() => {
		clearInterval(sampler);
		player?.close();
		connection.close();
	});
	const decoders = [player.video, player.audio];
	const failed = new Promise<never>((_, reject) => {
		for (const decoder of decoders) {
			cleanup.push(decoder.out.error.subscribe((err) => err && reject(err)));
		}
	});
	return { first_frame_ms: await Promise.race([first, failed]) };
}

// Reads the id strip from a clone of the painted frame, so the renderer can close its own copy.
function probe(frame: VideoFrame): void {
	const at: [number, number] = [now(), performance.now()];
	const clone = frame.clone();
	const rect = { x: 0, y: 0, width: clone.codedWidth & ~1, height: STRIP };
	const buffer = new Uint8Array(clone.allocationSize({ rect }));
	void clone
		.copyTo(buffer, { rect })
		.then((layout) => {
			const row = STRIP / 2;
			const luma = (x: number) => {
				const format = clone.format ?? "";
				if (format.startsWith("I4") || format.startsWith("NV12"))
					return buffer[layout[0].offset + row * layout[0].stride + x];
				// RGBA and BGRA alike: green dominates luma either way.
				return buffer[layout[0].offset + row * layout[0].stride + x * 4 + 1];
			};
			const block = rect.width / BLOCKS;
			let bits = 0;
			for (let bit = 0; bit < BLOCKS; bit++)
				bits = (bits << 1) | (luma(Math.floor(bit * block + block / 2)) > 128 ? 1 : 0);
			const id = bits >> CHECK_BITS;
			if ((bits & ((1 << CHECK_BITS) - 1)) === checksum(id) && !presentedAt.has(id)) presentedAt.set(id, at);
		})
		.finally(() => clone.close());
}

/** Watcher counters, read at the window's edges. The queue maximum restarts at each read. */
function watched() {
	const stats = player?.video.out.stats.peek();
	const snapshot = { ...decoder };
	decoder.queue_max = 0;
	return {
		presented,
		received: stats?.frameCount ?? 0,
		bytes: stats?.bytesReceived ?? 0,
		decoder: snapshot,
		heap: heap.length ? heap[heap.length - 1][1] : null,
	};
}

/** Heap samples and long tasks inside a window, on this page's clock. */
function window_(start: number, end: number) {
	return { heap: heap.filter(([at]) => at >= start && at < end), long_tasks: longTasks(start, end) };
}

/** Wall-clock presentation times by frame id inside a page-clock window; probed pass only. */
function presentations(start: number, end: number): [number, number][] {
	return [...presentedAt].filter(([, [, at]]) => at >= start && at < end).map(([id, [wall]]) => [id, wall]);
}

async function stop(): Promise<void> {
	for (const close of cleanup.splice(0)) close();
}

declare global {
	interface Window {
		media: {
			publish: typeof publish;
			published: typeof published;
			paints: typeof paints;
			watch: typeof watch;
			watched: typeof watched;
			window: typeof window_;
			presentations: typeof presentations;
			now: () => number;
			stop: typeof stop;
		};
	}
}
window.media = {
	publish,
	published,
	paints,
	watch,
	watched,
	window: window_,
	presentations,
	now: () => performance.now(),
	stop,
};
