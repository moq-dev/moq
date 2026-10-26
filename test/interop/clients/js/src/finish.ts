// Finite media fixture and finish observations for the existing browser client.
import { Micro, Milli, Nano, sleep } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Moq from "@moq/net";
import { CatalogProducer } from "@moq/publish";
import { Effect } from "@moq/signals";
import { readFinish } from "../../js-native/finish";

/** How long the publisher keeps its finished broadcast up for the late readers. */
const LINGER = Milli(10_000);

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
		const broadcastPublication = origin.createBroadcast(Moq.Path.from(path));
		const broadcast = broadcastPublication.broadcast;
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
		broadcastPublication.announce();
		// Keep the finished broadcast up for the late readers, then take it offline.
		state.lingerUs = LINGER * 1000;
		state.ready = true;
		await sleep(Nano.fromMilli(LINGER));
		broadcast.close();
		const healthPublication = origin.createBroadcast(Moq.Path.from(`${path}.health`));
		const health = healthPublication.broadcast;
		const healthTrack = health.createTrack("live");
		const group = healthTrack.appendGroup();
		group.writeFrame({ payload: new TextEncoder().encode("session-alive") });
		group.close();
		healthTrack.close();
		healthPublication.announce();
		state.ended = true;
		await ack;
		health.close();
		state.result = "finish acknowledged";
	})().catch((error) => {
		state.error = error instanceof Error ? error.message : String(error);
	});
}
