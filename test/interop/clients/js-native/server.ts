/** Controlled real QMux peers for reverse GOAWAY. */
import assert from "node:assert/strict";
import { createInterface } from "node:readline";
import * as Moq from "@moq/net";
import * as Extensions from "../../../../js/net/src/connection/extensions.ts";
import { exchangeSetup } from "../../../../js/net/src/connection/handshake.ts";
import { Connection } from "../../../../js/net/src/ietf/connection.ts";
import { GoAway } from "../../../../js/net/src/ietf/goaway.ts";
import * as Ietf from "../../../../js/net/src/ietf/index.ts";
import Qmux, { selectSubprotocol } from "../../../../js/qmux/src/index.ts";

console.debug = console.error;
console.info = console.error;
const [mode, name] = process.argv.slice(2);
assert(name && mode === "goaway", "usage: server.ts goaway VERSION");
const version = (0xff000000 + Number(name.split("-").at(-1))) as Ietf.IetfVersion;
const alpns = [`moqt-${Number(version) - 0xff000000}`];
const extensions = Extensions.offered();
const origin = new Moq.Origin.Producer();
const broadcastPublication = origin.createBroadcast(Moq.Path.from("handover"));
const broadcast = broadcastPublication.broadcast;
const video = broadcast.createTrack("video");
broadcastPublication.announce({ epoch: Moq.Epoch.mint() });
const markerPublication = origin.createBroadcast(Moq.Path.from("replacement"));
const marker = markerPublication.broadcast;
const ready = marker.createTrack("ready");
const markerGroup = ready.appendGroup();
markerGroup.writeFrame({ payload: new Uint8Array([42]) });
markerGroup.finish();
markerPublication.announce();

type Socket = {
	protocol: string;
	url: string;
	receive?: (data: Uint8Array) => void;
	end?: (code: number, reason: string) => void;
};

function listen(publish: Moq.Origin.Consumer) {
	const accepted = Promise.withResolvers<Awaited<ReturnType<typeof accept>>>();
	const server = Bun.serve<Socket>({
		hostname: "127.0.0.1",
		port: 0,
		fetch(request, server) {
			const protocol = selectSubprotocol(request.headers.get("sec-websocket-protocol"), {
				protocols: alpns,
				versions: Object.fromEntries(alpns.map((alpn) => [alpn, "qmux-01"])),
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
				void accept(quic, new URL(socket.data.url), publish).then(accepted.resolve, accepted.reject);
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

async function accept(quic: WebTransport, url: URL, publish: Moq.Origin.Consumer) {
	const setup = await exchangeSetup(quic, version, "interop-js-server", { client: false, extensions });
	return {
		control: setup.control,
		connection: new Connection({
			url,
			quic,
			version,
			client: false,
			publish,
			extensions,
			...setup,
		}),
	};
}

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
	console.error("js goaway redirect restarted");
} finally {
	lines.close();
	initial?.connection.abort();
	replacement?.connection.abort();
	first.server.stop(true);
	second.server.stop(true);
	origin.close();
}
