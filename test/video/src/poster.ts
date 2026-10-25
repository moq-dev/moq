import { Micro, Milli } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import { Legacy } from "@moq/hang/container";
import { Origin, Path } from "@moq/net";
import { Player } from "@moq/watch";

// Real WebCodecs and rendering, with publication controlled independently of wall time.
async function poster(audio: boolean, buffered: boolean) {
	const origin = new Origin.Producer();
	const name = Path.from("poster");
	const producer = origin.createBroadcast(name);
	const track = producer.createTrack("video");
	const group = track.appendGroup();
	const audioTrack = audio ? producer.createTrack("audio") : undefined;
	const audioGroup = audioTrack?.appendGroup();
	const catalog: Catalog.Root = {
		video: {
			renditions: {
				video: Catalog.VideoConfigSchema.parse({
					codec: "vp8",
					codedWidth: 64,
					codedHeight: 32,
					container: { kind: "legacy" },
				}),
			},
		},
		...(audio && {
			audio: {
				renditions: {
					audio: Catalog.AudioConfigSchema.parse({
						codec: "opus",
						sampleRate: 48000,
						numberOfChannels: 1,
						container: { kind: "legacy" },
					}),
				},
			},
		}),
	};
	producer.announce();
	const canvas = document.createElement("canvas");
	document.body.append(canvas);
	const player = new Player({
		origin,
		name,
		catalogFormat: "manual",
		catalog,
		video: { canvas, visible: "always", backend: "2d" },
		delay: Milli(100),
		buffer: Milli(buffered ? 1000 : 0),
	});
	const chunks = new Map<number, Uint8Array>();
	const encoder = new VideoEncoder({
		output(chunk) {
			const data = new Uint8Array(chunk.byteLength);
			chunk.copyTo(data);
			chunks.set(chunk.timestamp, data);
		},
		error(error) {
			throw error;
		},
	});
	encoder.configure({ codec: "vp8", width: 64, height: 32, bitrate: 100_000, latencyMode: "realtime" });
	const picture = new OffscreenCanvas(64, 32);
	const ctx = picture.getContext("2d");
	if (!ctx) throw new Error("no encoder canvas context");
	for (const timestamp of [0, 40_000, 100_000, 140_000]) {
		ctx.fillStyle = timestamp === 0 ? "red" : "blue";
		ctx.fillRect(0, 0, 64, 32);
		const frame = new VideoFrame(picture, { timestamp });
		encoder.encode(frame, { keyFrame: true });
		frame.close();
	}
	await encoder.flush();
	encoder.close();
	const push = (timestamp: number) => {
		const payload = chunks.get(timestamp);
		if (!payload) throw new Error("missing encoded frame");
		group.writeFrame({ payload: Legacy.encodeFrame(payload, Micro(timestamp)) });
	};
	return {
		push,
		async audio() {
			if (!audioGroup) throw new Error("no audio track");
			const encode = new AudioEncoder({
				output(chunk) {
					const data = new Uint8Array(chunk.byteLength);
					chunk.copyTo(data);
					audioGroup.writeFrame({ payload: Legacy.encodeFrame(data, Micro(chunk.timestamp)) });
				},
				error(error) {
					throw error;
				},
			});
			encode.configure({ codec: "opus", sampleRate: 48000, numberOfChannels: 1, bitrate: 64_000 });
			for (let i = 0; i < 12; i++) {
				const frame = new AudioData({
					format: "f32-planar",
					sampleRate: 48000,
					numberOfFrames: 960,
					numberOfChannels: 1,
					timestamp: i * 20_000,
					data: new Float32Array(960),
				});
				encode.encode(frame);
				frame.close();
			}
			await encode.flush();
			encode.close();
		},
		state() {
			const context = canvas.getContext("2d");
			if (!context) throw new Error("no renderer canvas context");
			const rgba = context.getImageData(32, 16, 1, 1).data;
			return {
				timestamp: player.renderer.out.frame.peek()?.timestamp,
				playing: player.sync.playing.peek(),
				color: rgba[0] > 160 ? "red" : rgba[2] > 160 ? "blue" : "black",
				audioTimestamp: player.audio.out.timestamp.peek(),
				audioState: player.audio.out.context.peek()?.state,
			};
		},
		close() {
			player.close();
			group.close();
			audioGroup?.close();
			producer.close();
			origin.close();
			canvas.remove();
		},
	};
}

declare global {
	interface Window {
		poster: Awaited<ReturnType<typeof poster>>;
		startPoster(audio: boolean, buffered: boolean): Promise<void>;
	}
}
window.startPoster = async (audio, buffered) => {
	window.poster = await poster(audio, buffered);
};
