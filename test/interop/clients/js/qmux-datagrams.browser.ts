/** A QMux session over Chromium's native `WebSocketStream` sends every datagram on an idle socket. */
import { expect, test } from "bun:test";
import { createServer } from "vite";
import Session, { selectSubprotocol } from "../../../../js/qmux/src/index.ts";
import { BunSocketStream } from "../../../qmux/bun-socket.ts";
import { launch } from "./harness";

const PROTOCOL = "qmux-datagrams";
const COUNT = 100;

test("native WebSocketStream sends every datagram written back to back", async () => {
	let accepted = Promise.withResolvers<Session>();
	const socket = Bun.serve<{ stream: BunSocketStream }>({
		port: 0,
		hostname: "127.0.0.1",
		fetch(req, server) {
			const protocol = selectSubprotocol(req.headers.get("sec-websocket-protocol"), {
				protocols: [PROTOCOL],
				versions: { [PROTOCOL]: "qmux-01" },
				requireProtocol: true,
			});
			if (!protocol) return new Response("no supported protocol", { status: 400 });
			const stream = new BunSocketStream(protocol);
			accepted.resolve(Session.accept(stream, { protocol }));
			if (server.upgrade(req, { data: { stream }, headers: { "sec-websocket-protocol": protocol } })) return;
			return new Response("upgrade failed", { status: 500 });
		},
		websocket: {
			open: (ws) => ws.data.stream.attach(ws),
			message: (ws, message) => ws.data.stream.push(message),
			close: (ws, code, reason) => ws.data.stream.finish(code, reason),
		},
	});
	const server = await createServer({
		configFile: false,
		root: `${import.meta.dir}/qmux-datagrams`,
		server: { host: "127.0.0.1", port: 0 },
	});
	await server.listen();
	const browser = await launch();
	try {
		const page = await browser.newPage();
		const errors: string[] = [];
		page.on("pageerror", (error) => errors.push(error.message));
		await page.goto(server.resolvedUrls?.local[0] ?? "");

		// Each write awaited, as WebTransport callers do, then all at once.
		for (const burst of [false, true]) {
			accepted = Promise.withResolvers<Session>();
			const sent = page.evaluate(
				async ([url, protocol, count, burst]) =>
					(await import(`${location.origin}/main.ts`)).run(url, protocol, count, burst),
				[`ws://127.0.0.1:${socket.port}/`, PROTOCOL, COUNT, burst] as const,
			);
			const session = await accepted.promise;
			const end = await session.incomingUnidirectionalStreams.getReader().read();
			await end.value?.getReader().read();
			await sent;

			// The socket is ordered, so every datagram the browser kept arrived ahead of the
			// stream that marks the end, and a clean close leaves them readable.
			session.close();
			const received: number[] = [];
			for await (const datagram of session.datagrams.readable) received.push(...datagram);
			expect({ burst, received }).toEqual({ burst, received: Array.from({ length: COUNT }, (_, i) => i) });
		}
		expect(errors).toEqual([]);
	} finally {
		await browser.close();
		await server.close();
		socket.stop(true);
	}
}, 30_000);
