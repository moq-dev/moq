// Finite media fixture and finish observations for the existing browser client.
import { Instant, Micro, Milli, Nano, now } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Moq from "@moq/net";
import { CatalogProducer } from "@moq/publish";
import { Effect } from "@moq/signals";
import { readFinish } from "../../js-native/finish";

interface State {
	ready: boolean;
	ended: boolean;
	lingerUs?: number;
	result?: string;
	error?: string;
	read(): void;
	acknowledge(): void;
	close(): void;
}

declare global {
	interface Window {
		interopFinish?: State;
	}
}

export function finish(
	role: string,
	url: string,
	path: string,
	version: string | null,
	frames: number,
	timeout: number,
): void {
	let releaseRead = () => {};
	let releaseAck = () => {};
	const read = new Promise<void>((resolve) => {
		releaseRead = resolve;
	});
	const ack = new Promise<void>((resolve) => {
		releaseAck = resolve;
	});
	const origin = new Moq.Origin.Producer();
	let connection: Moq.Connection.Established | undefined;
	const state: State = {
		ready: false,
		ended: false,
		read: () => releaseRead(),
		acknowledge: () => releaseAck(),
		close: () => {
			connection?.close();
			origin.close();
		},
	};
	window.interopFinish = state;
	void (async () => {
		connection = await Moq.Connection.connect({
			url: new URL(url),
			consume: origin,
			publish: origin.consume(),
			websocket: { enabled: false },
			...(version ? { versions: [version] } : {}),
		});
		if (role === "finish-subscribe") {
			state.ready = true;
			await read;
			state.result = await readFinish(origin, path, frames, Milli(timeout));
			return;
		}
		const broadcast = origin.createBroadcast(Moq.Path.from(path));
		const config: Catalog.VideoConfig = {
			codec: "avc1.42001e",
			codedWidth: Catalog.u53(320),
			codedHeight: Catalog.u53(240),
			container: { kind: "legacy" },
		};
		const track = broadcast.createTrack("video", {
			timescale: Moq.Time.Timescale.MICRO,
			maxAge: Milli(Number.MAX_SAFE_INTEGER),
			priority: Catalog.PRIORITY.video,
		});
		const media = new Container.Legacy.Producer(track, new Container.Legacy.Format(config));
		let encodingError: Error | undefined;
		const encoder = new VideoEncoder({
			output: (chunk) => media.encode(chunk, Micro(chunk.timestamp), chunk.type === "key"),
			error: (error) => {
				encodingError = error;
			},
		});
		try {
			encoder.configure({
				codec: config.codec,
				width: 320,
				height: 240,
				bitrate: 300_000,
				framerate: 30,
				avc: { format: "annexb" },
			});
			const canvas = new OffscreenCanvas(320, 240);
			const context = canvas.getContext("2d");
			if (!context) throw new Error("no fixture canvas");
			for (let index = 0; index < frames; index++) {
				context.fillStyle = `rgb(${index * 20},80,160)`;
				context.fillRect(0, 0, 320, 240);
				const frame = new VideoFrame(canvas, { timestamp: Math.round((index * 1_000_000) / 30) });
				try {
					encoder.encode(frame, { keyFrame: index === 0 });
				} finally {
					frame.close();
				}
			}
			await encoder.flush();
			if (encodingError) throw encodingError;
		} finally {
			encoder.close();
		}
		media.close();
		const catalog = new CatalogProducer();
		catalog.mutate((root) => {
			root.video = { renditions: { video: config } };
		});
		const serving = new Effect();
		catalog.serve(
			broadcast.createTrack(Catalog.TRACK, {
				timescale: Moq.Time.Timescale.MILLI,
				maxAge: Milli(Number.MAX_SAFE_INTEGER),
				priority: Catalog.PRIORITY.catalog,
			}),
			serving,
		);
		serving.close();
		broadcast.announce();
		const started = now();
		const finishing = broadcast.finish();
		const linger = Nano.fromMilli(finishing.linger);
		if (linger <= 0) throw new Error("finished media has no linger");
		state.lingerUs = finishing.linger * 1000;
		state.ready = true;
		await finishing.wait();
		if (Instant.durationSince(now(), started) < linger) throw new Error("finish wait ended before its linger");
		const health = origin.createBroadcast(Moq.Path.from(`${path}.health`));
		const healthTrack = health.createTrack("live");
		const group = healthTrack.appendGroup();
		group.writeFrame({ payload: new TextEncoder().encode("session-alive") });
		group.close();
		healthTrack.close();
		health.announce();
		state.ended = true;
		await ack;
		health.close();
		state.result = "finish acknowledged";
	})().catch((error) => {
		state.error = error instanceof Error ? error.message : String(error);
	});
}
