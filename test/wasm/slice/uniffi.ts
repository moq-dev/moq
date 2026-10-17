// The slice bound by UniFFI over ubrn's wasm2 player, behind the harness adapter.
import type { Frame, Open, RemoteGroup } from "./adapter.ts";
import {
	MoqBroadcastProducer,
	MoqBrowserSession,
	type MoqFrame,
	type MoqGroupConsumer,
	type MoqGroupConsumerLike,
	noop,
	noopAsync,
	uniffiInitAsync,
	yieldOnce,
} from "./dist/uniffi/index.ts";

// The harness hands whole-buffer payloads, so taking `.buffer` adds no JS copy.
const lower = (bytes: Uint8Array, timestampUs?: bigint): MoqFrame => ({
	payload: bytes.buffer as ArrayBuffer,
	timestampUs,
});
const lift = (frame: MoqFrame | undefined): Frame | undefined =>
	frame && { payload: new Uint8Array(frame.payload), timestampUs: frame.timestampUs };
const opts = (signal?: AbortSignal) => (signal ? { signal } : undefined);
const remote = (group: MoqGroupConsumerLike): RemoteGroup => ({
	sequence: () => group.sequence(),
	read: async (signal) => lift(await group.readFrame(opts(signal))),
});

export const open: Open = async (wasm) => {
	await uniffiInitAsync(wasm);
	return {
		noop,
		noopAsync: () => noopAsync(),
		yieldOnce: () => yieldOnce(),
		group(sequence) {
			const broadcast = new MoqBroadcastProducer();
			const track = broadcast.publishTrack("data");
			const group = sequence === undefined ? track.appendGroup() : track.createGroup(sequence);
			const consumer = group.consume();
			return {
				sequence: () => consumer.sequence(),
				write: (bytes, timestampUs = 0n) => group.writeFrame(lower(bytes, timestampUs)),
				read: async (signal) => lift(await consumer.readFrame(opts(signal))),
				// The generated `*Like` types omit uniffiDestroy; only the class declares it.
				free: () => (consumer as MoqGroupConsumer).uniffiDestroy(),
				close: () => {
					group.finish();
					track.finish();
				},
			};
		},
		async connect(url, fingerprint, version) {
			const session = await MoqBrowserSession.connect(url, fingerprint, version);
			return {
				async broadcast(path) {
					const broadcast = await session.announcedBroadcast(path);
					return {
						async subscribe(name) {
							const track = await broadcast.subscribeTrack(name);
							return { recvGroup: async (signal) => remote(await track.recvGroup(opts(signal))) };
						},
					};
				},
				publish(path) {
					const track = session.publishTrack(path, "data");
					return {
						group(sequence) {
							const group = track.createGroup(sequence);
							return {
								write: (bytes, timestampUs) => group.writeFrame(lower(bytes, timestampUs)),
								finish: () => group.finish(),
							};
						},
					};
				},
				cancel: () => session.cancel(),
			};
		},
	};
};
