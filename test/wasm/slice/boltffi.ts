// The slice bound by BoltFFI, behind the harness adapter.
import type { Frame, Open, RemoteGroup } from "./adapter.ts";
import init, {
	MoqBroadcastProducer,
	MoqBrowserSession,
	type MoqFrame,
	type MoqGroupConsumer,
	noop,
	noopAsync,
	yieldOnce,
} from "./dist/boltffi/moq_ffi_gen.js";

const lift = (frame: MoqFrame | null): Frame | undefined =>
	frame ? { payload: frame.payload, timestampUs: frame.timestampUs ?? undefined } : undefined;
const opts = (signal?: AbortSignal) => (signal ? { signal } : undefined);
const remote = (group: MoqGroupConsumer): RemoteGroup => ({
	sequence: () => group.sequence(),
	read: async (signal) => lift(await group.readFrame(opts(signal))),
});

export const open: Open = async (wasm) => {
	await init(await fetch(wasm));
	return {
		noop,
		noopAsync: () => noopAsync(),
		yieldOnce: () => yieldOnce(),
		group(sequence) {
			const broadcast = MoqBroadcastProducer.new();
			const track = broadcast.publishTrack("data");
			const group = sequence === undefined ? track.appendGroup() : track.createGroup(sequence);
			const consumer = group.consume();
			return {
				sequence: () => consumer.sequence(),
				write: (bytes, timestampUs = 0n) => group.writeFrame({ payload: bytes, timestampUs }),
				read: async (signal) => lift(await consumer.readFrame(opts(signal))),
				free: () => consumer.dispose(),
				close: () => {
					group.finish();
					track.finish();
				},
			};
		},
		async connect(url, fingerprint, version) {
			// BoltFFI types an async constructor's result as nullable (boltffi#95).
			const session = await MoqBrowserSession.connect(url, fingerprint, version);
			if (!session) throw new Error("BoltFFI's async constructor returned null");
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
								write: (bytes, timestampUs) => group.writeFrame({ payload: bytes, timestampUs }),
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
