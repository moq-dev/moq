/** Controlled real QMux peers for reverse GOAWAY and the JS subscriber's credit wait. */
import assert from "node:assert/strict";
import { createInterface } from "node:readline";
import { Milli } from "@moq/clock";
import * as Moq from "@moq/net";
import * as Extensions from "../../../../js/net/src/connection/extensions.ts";
import { exchangeSetup } from "../../../../js/net/src/connection/handshake.ts";
import { Connection } from "../../../../js/net/src/ietf/connection.ts";
import { GoAway } from "../../../../js/net/src/ietf/goaway.ts";
import * as Ietf from "../../../../js/net/src/ietf/index.ts";
import * as Lite from "../../../../js/net/src/lite/index.ts";
import { Stream } from "../../../../js/net/src/stream.ts";
import Qmux, { selectSubprotocol } from "../../../../js/qmux/src/index.ts";

console.debug = console.error;
console.info = console.error;
const [mode, name] = process.argv.slice(2);
assert(name && ["goaway", "credit"].includes(mode), "usage: server.ts goaway|credit VERSION");
const version = (0xff000000 + Number(name.split("-").at(-1))) as Ietf.IetfVersion;
const legacy = version === Ietf.Version.DRAFT_14 || version === Ietf.Version.DRAFT_16;
// JS offers draft 14 behind `moql`, negotiated in SETUP; Rust offers its own `moq-00` ALPN.
const alpns = version === Ietf.Version.DRAFT_14 ? ["moql", "moq-00"] : [`moqt-${Number(version) - 0xff000000}`];
const extensions = Extensions.offered();
const origin = new Moq.Origin.Producer();
const broadcast = origin.createBroadcast(Moq.Path.from("handover"));
const firstTrack = broadcast.createTrack("first");
const secondTrack = broadcast.createTrack("second");
const video = broadcast.createTrack("video");
broadcast.announce({ epoch: Moq.Epoch.mint() });
const marker = origin.createBroadcast(Moq.Path.from("replacement"));
const ready = marker.createTrack("ready");
const markerGroup = ready.appendGroup();
markerGroup.writeFrame({ payload: new Uint8Array([42]) });
markerGroup.finish();
marker.announce();

type Socket = {
	protocol: string;
	url: string;
	receive?: (data: Uint8Array) => void;
	end?: (code: number, reason: string) => void;
};

function listen(publish: Moq.Origin.Consumer, window?: bigint) {
	const accepted = Promise.withResolvers<{ connection: Connection; control: Stream }>();
	const server = Bun.serve<Socket>({
		hostname: "127.0.0.1",
		port: 0,
		fetch(request, server) {
			const protocol = selectSubprotocol(request.headers.get("sec-websocket-protocol"), {
				protocols: alpns,
				versions: Object.fromEntries(alpns.map((alpn) => [alpn, legacy ? "qmux-00" : "qmux-01"])),
				requireProtocol: true,
			});
			if (!protocol) return new Response("no protocol", { status: 400 });
			if (
				server.upgrade(request, {
					headers: { "Sec-WebSocket-Protocol": protocol },
					data: { protocol, url: request.url },
				})
			)
				return;
			return new Response("upgrade refused", { status: 400 });
		},
		websocket: {
			open(socket) {
				const closed = Promise.withResolvers<{ closeCode: number; reason: string }>();
				const readable = new ReadableStream<Uint8Array>({
					start(controller) {
						socket.data.receive = (data) => controller.enqueue(data);
						socket.data.end = (closeCode, reason) => {
							controller.close();
							closed.resolve({ closeCode, reason });
						};
					},
				});
				const writable = new WritableStream<Uint8Array | string>({
					write(data) {
						socket.send(data);
					},
				});
				const quic = Qmux.accept(
					{
						url: socket.data.url,
						opened: Promise.resolve({ readable, writable, protocol: socket.data.protocol, extensions: "" }),
						closed: closed.promise,
						close() {
							socket.close();
						},
					},
					{ protocol: socket.data.protocol },
				);
				void accept(quic, new URL(socket.data.url), publish, window).then(accepted.resolve, accepted.reject);
			},
			message(socket, data) {
				assert(typeof data !== "string");
				socket.data.receive?.(new Uint8Array(data));
			},
			close(socket, code, reason) {
				socket.data.end?.(code, reason);
			},
		},
	});
	return { server, url: `http://127.0.0.1:${server.port}/`, accepted: accepted.promise };
}

async function accept(quic: WebTransport, url: URL, publish: Moq.Origin.Consumer, window?: bigint) {
	if (!legacy) {
		const setup = await exchangeSetup(quic, version, "interop-js-server", { client: false, extensions });
		return {
			control: setup.control,
			connection: new Connection({
				url,
				quic,
				version,
				client: false,
				maxRequestId: 0n,
				publish,
				extensions,
				...setup,
			}),
		};
	}
	const control = await Stream.accept(quic, version);
	assert(control, "missing SETUP");
	assert.equal(await control.reader.u53(), Lite.StreamId.ClientCompat);
	const client = await Ietf.ClientSetup.decode(control.reader, version);
	const parameters = new Ietf.SetupOptions();
	parameters.setVarint(Ietf.SetupOption.MaxRequestId, Ietf.initialMaxRequestId(true, window));
	Extensions.intoSetup(parameters, extensions, version);
	await control.writer.u53(Lite.StreamId.ServerCompat);
	await new Ietf.ServerSetup({ version, parameters }).encode(control.writer, version);
	return {
		control,
		connection: new Connection({
			url,
			quic,
			control,
			version,
			client: false,
			publish,
			extensions,
			requestWindow: window,
			maxRequestId: client.parameters.getVarint(Ietf.SetupOption.MaxRequestId) ?? 0n,
			solicit: Ietf.solicitFromSetup(client.parameters),
		}),
	};
}

if (mode === "credit") {
	assert(legacy, "request credit exists on drafts 14 and 16");
	// Namespace discovery holds one slot; the first track holds the other.
	const peer = listen(origin.consume(), 2n);
	const received = new Moq.Origin.Producer();
	const client = await Moq.Connection.connect({
		url: new URL(peer.url),
		versions: [name],
		websocket: { delay: Milli(0) },
		consume: received,
	});
	const server = await peer.accepted;
	const request = received.request(Moq.Path.from("handover"), { announced: true });
	try {
		let remote = request.active.peek();
		while (!remote) {
			await request.active.changed();
			remote = request.active.peek();
		}
		const first = remote.track("first").subscribe();
		await first.info();
		const second = remote.track("second").subscribe();
		const queued = second.info();
		try {
			const probe = firstTrack.appendGroup();
			probe.writeFrame({ payload: new Uint8Array([1]) });
			probe.finish();
			const data = await first.recvGroup();
			assert(data);
			assert.deepEqual((await data.readFrame())?.payload, new Uint8Array([1]));
			data.close();
			first.close();
			await queued;
			const next = secondTrack.appendGroup();
			next.writeFrame({ payload: new Uint8Array([2]) });
			next.finish();
			const resumed = await second.recvGroup();
			assert(resumed);
			assert.deepEqual((await resumed.readFrame())?.payload, new Uint8Array([2]));
			resumed.close();
			console.error("js request credit resumed");
		} finally {
			first.close();
			second.close();
		}
	} finally {
		request.close();
		await client.close();
		server.connection.abort();
		peer.server.stop(true);
		received.close();
		origin.close();
	}
} else {
	const scoped = origin.scope(Moq.Path.empty(), new Moq.Path.Patterns([Moq.Path.Pattern.literal("handover")]));
	const first = listen(scoped.consume());
	const second = listen(origin.consume());
	process.stdout.write(`${JSON.stringify([first.url, second.url])}\n`);
	const lines = createInterface({ input: process.stdin });
	const commands = lines[Symbol.asyncIterator]();
	let initial: Awaited<typeof first.accepted> | undefined;
	let replacement: Awaited<typeof second.accepted> | undefined;
	try {
		initial = await first.accepted;
		const demand = video.demand();
		while (demand.wanted.peek() === undefined) await demand.wanted.changed();
		const group = video.appendGroup();
		group.writeFrame({ payload: new Uint8Array(32) });
		group.finish();
		assert.equal((await commands.next()).value, "first group");
		await initial.control.writer.u53(GoAway.id);
		await new GoAway({
			newSessionUri: second.url,
			requestId: version === Ietf.Version.DRAFT_18 ? 0n : undefined,
		}).encode(initial.control.writer, version);
		replacement = await second.accepted;
		assert.equal((await commands.next()).value, "replacement ready");
		initial.connection.abort();
		const next = video.appendGroup();
		next.writeFrame({ payload: new Uint8Array(32).fill(1) });
		next.finish();
		assert.equal((await commands.next()).value, "done");
		console.error("js goaway redirect resumed");
	} finally {
		lines.close();
		initial?.connection.abort();
		replacement?.connection.abort();
		first.server.stop(true);
		second.server.stop(true);
		origin.close();
	}
}
