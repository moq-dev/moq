/** Compare eager and cold lazy fallback startup with real WebSocket negotiation. */
import { basename } from "node:path";
import { gzipSync } from "node:zlib";
import { Session, selectSubprotocol, type WebSocketStreamCloseEvent, type WebSocketStreamCloseInfo } from "@moq/qmux";
import { type Browser, chromium } from "playwright";

/** Per-connection state from the upgrade; `open` attaches the frame feed. */
interface Peer {
	url: string;
	protocol: string;
	closed: PromiseWithResolvers<WebSocketStreamCloseEvent>;
	incoming?: ReadableStreamDefaultController<Uint8Array>;
}

const build = await Bun.build({
	entrypoints: [`${import.meta.dir}/qmux-eager.ts`, `${import.meta.dir}/qmux-lazy.ts`],
	target: "browser",
	splitting: true,
	minify: true,
});
if (!build.success) throw new AggregateError(build.logs, "qmux benchmark build failed");
const files = new Map(
	await Promise.all(
		build.outputs.map(async (file) => [basename(file.path), gzipSync(await file.arrayBuffer())] as const),
	),
);
const server = Bun.serve<Peer>({
	port: 0,
	fetch(request, server) {
		const path = new URL(request.url).pathname;
		if (path === "/qmux") {
			const protocol = selectSubprotocol(request.headers.get("Sec-WebSocket-Protocol"), {});
			if (!protocol) return new Response("unsupported QMux protocol", { status: 400 });
			const data: Peer = { url: request.url, protocol, closed: Promise.withResolvers() };
			if (server.upgrade(request, { data, headers: { "Sec-WebSocket-Protocol": protocol } })) return;
			return new Response("upgrade required", { status: 400 });
		}
		const code = files.get(path.slice(1));
		if (code)
			return new Response(code, { headers: { "Content-Type": "text/javascript", "Content-Encoding": "gzip" } });
		const entry = path === "/lazy" ? "qmux-lazy.js" : "qmux-eager.js";
		return new Response(`<script type="module" src="/${entry}"></script>`, {
			headers: { "Content-Type": "text/html" },
		});
	},
	websocket: {
		// Accept the upgraded socket like a real server, so Session sends its transport parameters first.
		open(ws) {
			const peer = ws.data;
			Session.accept({
				url: peer.url,
				opened: Promise.resolve({
					protocol: peer.protocol,
					extensions: "",
					readable: new ReadableStream<Uint8Array>({
						start: (controller) => {
							peer.incoming = controller;
						},
					}),
					writable: new WritableStream<Uint8Array>({
						write: (bytes) => {
							ws.send(bytes);
						},
					}),
				}),
				closed: peer.closed.promise,
				close: (info?: WebSocketStreamCloseInfo) => ws.close(info?.closeCode, info?.reason),
			});
		},
		message(ws, message) {
			if (typeof message === "string") throw new Error("QMux frames are binary");
			if (!ws.data.incoming) throw new Error("QMux frame before the socket opened");
			ws.data.incoming.enqueue(message);
		},
		close(ws, code, reason) {
			ws.data.incoming?.close();
			ws.data.closed.resolve({ closeCode: code, reason });
		},
	},
});
let browser: Browser | undefined;
try {
	browser = await chromium.launch({ channel: "chromium", headless: true });
	console.log("round,latency_ms,variant,entry_gzip_bytes,connect_ms");
	for (let round = 1; round <= 3; round++) {
		for (const latency of [0, 50]) {
			for (const variant of round === 2 ? ["lazy", "eager"] : ["eager", "lazy"]) {
				const context = await browser.newContext();
				try {
					const page = await context.newPage();
					const cdp = await context.newCDPSession(page);
					await cdp.send("Network.enable");
					await cdp.send("Network.setCacheDisabled", { cacheDisabled: true });
					await cdp.send("Network.emulateNetworkConditions", {
						offline: false,
						latency,
						downloadThroughput: 1_250_000,
						uploadThroughput: 1_250_000,
					});
					await page.goto(`http://localhost:${server.port}/${variant}`);
					await page.waitForFunction(() => "measureQmux" in globalThis);
					const result = await page.evaluate(async (url) => {
						const bytes = performance
							.getEntriesByType("resource")
							.filter((entry) => entry.name.endsWith(".js"))
							.reduce((total, entry) => total + (entry as PerformanceResourceTiming).encodedBodySize, 0);
						const measure = (globalThis as unknown as { measureQmux(url: string): Promise<number> })
							.measureQmux;
						return { bytes, elapsed: await measure(url) };
					}, `http://localhost:${server.port}/qmux`);
					console.log(`${round},${latency},${variant},${result.bytes},${result.elapsed.toFixed(3)}`);
				} finally {
					await context.close();
				}
			}
		}
	}
} finally {
	server.stop(true);
	await browser?.close();
}
