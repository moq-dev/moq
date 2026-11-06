import * as Moq from "@moq/net";
import { Transport } from "@moq/p2p";

export interface Report {
	client: string;
	server: string;
	version: string;
	bytes: number;
	/** The subscriber's close error, or null for a clean close. */
	closed: string | null;
	peerStates: string[];
}

declare global {
	interface Window {
		checkTransport(): Promise<Report>;
	}
}

async function gathered(peer: RTCPeerConnection) {
	if (peer.iceGatheringState === "complete") return;
	await new Promise<void>((resolve) => {
		const changed = () => {
			if (peer.iceGatheringState !== "complete") return;
			peer.removeEventListener("icegatheringstatechange", changed);
			resolve();
		};
		peer.addEventListener("icegatheringstatechange", changed);
		changed();
	});
}

window.checkTransport = async () => {
	const left = new RTCPeerConnection({ iceServers: [] });
	const right = new RTCPeerConnection({ iceServers: [] });
	const protocol = "moq-lite-07-wip";
	const transport = Transport.connect({ peer: left, protocol });
	const incoming = new Promise<Transport>((resolve, reject) => {
		right.addEventListener(
			"datachannel",
			({ channel }) => {
				try {
					resolve(Transport.accept({ peer: right, channel, protocol }));
				} catch (error) {
					channel.close();
					reject(error);
				}
			},
			{ once: true },
		);
	});
	const publishing = new Moq.Origin.Producer();
	const consuming = new Moq.Origin.Producer();
	const path = Moq.Path.from("peer/data");
	const broadcastRoute = publishing.route(path);
	const broadcast = broadcastRoute.broadcast;
	const track = broadcast.createTrack("data", { timescale: Moq.Time.Timescale.MILLI });
	let client: Awaited<ReturnType<typeof Moq.Connection.connect>> | undefined;
	let server: Awaited<ReturnType<typeof Moq.Connection.accept>> | undefined;
	try {
		await left.setLocalDescription(await left.createOffer());
		await gathered(left);
		if (!left.localDescription) throw new Error("missing offer");
		await right.setRemoteDescription(left.localDescription);
		await right.setLocalDescription(await right.createAnswer());
		await gathered(right);
		if (!right.localDescription) throw new Error("missing answer");
		await left.setRemoteDescription(right.localDescription);
		const accepted = await incoming;
		const url = new URL("https://peer.invalid/");
		[client, server] = await Promise.all([
			Moq.Connection.connect({ url, transport, consume: consuming }),
			Moq.Connection.accept({ url, transport: accepted, publish: publishing.consume() }),
		]);
		const request = consuming.request(path);
		try {
			while (!request.active.peek()) await request.active.changed();
			const active = request.active.peek();
			if (!active) throw new Error("broadcast disappeared");
			const subscriber = active.track("data").subscribe().ordered();
			try {
				const demand = track.demand().wanted;
				while (demand.peek() === undefined) await demand.changed();
				const payload = Uint8Array.from({ length: 256 * 1024 }, (_, i) => i % 251);
				track.writeFrame({ payload, timestamp: Moq.Time.Timestamp.now() });
				const frame = await subscriber.readFrame();
				if (
					!frame ||
					frame.payload.byteLength !== payload.byteLength ||
					frame.payload.some((value, i) => value !== payload[i])
				) {
					throw new Error("data-channel frame payload changed");
				}
				subscriber.close();
				// Closing both ends at once makes the publisher's drain race the subscriber's
				// close, so the publisher closes and the subscriber observes its clean close.
				await server.close();
				const closed = await client.closed;
				return {
					client: client.transport,
					server: server.transport,
					version: client.version,
					bytes: frame.payload.byteLength,
					closed: closed?.message ?? null,
					peerStates: [left.signalingState, right.signalingState],
				};
			} finally {
				subscriber.close();
			}
		} finally {
			request.close();
		}
	} finally {
		client?.abort();
		server?.abort();
		transport.close();
		track.finish();
		track.close();
		broadcast.close();
		publishing.close();
		consuming.close();
		left.close();
		right.close();
	}
};
