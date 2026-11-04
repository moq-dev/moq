/**
 * Proves a relay's rejection reaches the page over WebTransport and WebSocket.
 * WebTransport.closed has to carry the relay's code; a WebSocket Connection has
 * to publish its MalformedAuthToken session error and stop reconnecting.
 *
 * Chromium treats the server's HTTP/3 control stream ending as fatal, so a server that ends it
 * alongside the CLOSE_WEBTRANSPORT_SESSION capsule turns the close into a bare connection error.
 * No Rust peer is that strict, which is why this check runs in a browser.
 *
 *     bun close.ts --url http://127.0.0.1:4443 [--timeout 20]
 *
 * @module
 */
import { parseArgs } from "node:util";
import { SessionCode } from "@moq/net";
import type { Page } from "playwright";
import { check, finishTraces, launch, open, pageUrl, serve, waitFor } from "./harness";
import type { CloseState } from "./src/contract";

const { values } = parseArgs({
	options: {
		url: { type: "string" },
		timeout: { type: "string", default: "20" },
	},
});

const url = values.url;
const timeoutMs = Number.parseFloat(values.timeout ?? "20") * 1000;
if (!url || !Number.isFinite(timeoutMs) || timeoutMs <= 0) {
	console.error("usage: close.ts --url U [--timeout S>0]");
	process.exit(2);
}

/** What the relay sends when its auth refuses a token that is not a JWT: moq-net's code and message. */
const EXPECTED = { closeCode: SessionCode.MalformedAuthToken, reason: "malformed auth token" };

async function readClose(page: Page): Promise<CloseState | undefined> {
	const state = await page.evaluate(() => document.body.dataset.interopClose);
	return state ? (JSON.parse(state) as CloseState) : undefined;
}

const server = serve();
const browser = await launch();

let code = 1;
try {
	for (const transport of ["webtransport", "websocket"]) {
		const [page, errors] = await open(
			browser,
			pageUrl(server.origin, "close", { url, transport }),
			`close-${transport}`,
			true,
		);
		const state = await waitFor(page, errors, readClose, {
			deadline: Date.now() + timeoutMs,
			description: `the refused ${transport} session to close`,
			predicate: (state) => state !== undefined,
			assertion: "close code",
		});
		check(
			state !== undefined &&
				"closeCode" in state &&
				state.closeCode === EXPECTED.closeCode &&
				(transport === "websocket" ? state.reason.includes(EXPECTED.reason) : state.reason === EXPECTED.reason),
			"close code",
			() => `expected ${JSON.stringify(EXPECTED)}, got ${JSON.stringify(state)}`,
		);
		console.error(`refused ${transport} session closed with ${JSON.stringify(state)}`);
		await page.close();
	}
	code = 0;
} finally {
	await finishTraces(code !== 0);
	await browser.close().catch(() => {});
	server.stop();
}
process.exit(code);
