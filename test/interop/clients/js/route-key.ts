/**
 * Proves a browser tab's route key to the relay: a lite-07 WebTransport session from `--engine`
 * proves the key in SETUP exactly when this browser build should, and the relay verifies it.
 *
 * The relay never acknowledges a proof, so this reads the relay's log. The page advertises an
 * implementation unique to this run, which the relay records on the session's `conn{...}` span
 * when it admits the session after the tab's SETUP; the relay's `peer proved its route key` debug
 * line (`moq_net::lite::setup`) on that connection means the proof verified, and a proof that fails
 * ends the session before it is admitted.
 *
 * Exits 77 with a `SKIP` line when this build cannot prove yet and sent no proof, as it must.
 *
 *     bun route-key.ts --url http://127.0.0.1:4443/?jwt=... --relay-log relay.log --engine firefox [--timeout 20]
 *
 * @module
 */
import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import { parseArgs, stripVTControlCharacters } from "node:util";
import { type Engine, finishTraces, launch, open, POLL_INTERVAL_MS, pageUrl, serve, sleep } from "./harness";

const { values } = parseArgs({
	options: {
		url: { type: "string" },
		"relay-log": { type: "string" },
		engine: { type: "string" },
		timeout: { type: "string", default: "20" },
	},
});

const url = values.url;
const relayLog = values["relay-log"];
const engine = values.engine;
const timeoutMs = Number.parseFloat(values.timeout ?? "20") * 1000;
if (!url || !relayLog || (engine !== "chromium" && engine !== "firefox") || !(timeoutMs > 0)) {
	console.error("usage: route-key.ts --url U --relay-log FILE --engine chromium|firefox [--timeout S>0]");
	process.exit(2);
}

/**
 * Why a tab on `engine` at `major` cannot prove its key yet, or `undefined` when it must. Such a
 * tab must send no proof, since the relay would end its session.
 */
function uncovered(engine: Engine, major: number): string | undefined {
	if (engine === "firefox") return "Firefox's exporter departs from the draft (a varint session ID in neqo)";
	return major >= 158 ? undefined : "Chromium ships its exporter in 158; the Playwright bump covers it";
}

/** The relay's log so far, without colors. */
const relay = () => stripVTControlCharacters(readFileSync(relayLog, "utf8"));

const server = serve();
const browser = await launch([], engine);
const version = browser.version();
const why = uncovered(engine, Number.parseInt(version, 10));
console.log(`BROWSER ${engine} ${version}`);

// Unique to this run, so no other session's SETUP is mistaken for this tab's. The relay records it
// escaped, as a quoted string.
const implementation = `interop-route-key/${randomUUID()}`;
const admitted = new RegExp(
	`conn\\{id=(\\d+) [^}]*implementation="${implementation}"[^}]*\\}.*negotiated version=(\\S+)`,
);

let code = 1;
try {
	const [page] = await open(browser, pageUrl(server.origin, "route-key", { url, implementation }), "route-key", true);
	const deadline = Date.now() + timeoutMs;
	let session: RegExpExecArray | null = null;
	while (!session) {
		const ended = await page.evaluate(() => document.body.dataset.interopRouteKey);
		if (ended) throw new Error(`the tab's session ended before the relay admitted it: ${ended}`);
		if (Date.now() > deadline) throw new Error(`the relay did not admit the tab's session within ${timeoutMs} ms`);
		session = admitted.exec(relay());
		if (!session) await sleep(POLL_INTERVAL_MS);
	}
	const [, id, negotiated] = session;
	if (negotiated !== "moq-lite-07-wip") throw new Error(`the tab's session negotiated ${negotiated}, not lite-07`);
	// Logged as the relay verifies the tab's SETUP, before it admits the session.
	const proven = new RegExp(`conn\\{id=${id}[ }].*peer proved its route key`).test(relay());
	console.log(
		`relay connection ${id}: ${proven ? "the tab proved its route key" : "the tab sent no route key proof"}`,
	);

	if (why === undefined && !proven) {
		console.error(`FAIL ${engine} ${version}: the tab sent no route key proof`);
	} else if (why !== undefined && proven) {
		console.error(`FAIL ${engine} ${version}: proved its route key, but ${why}; update this table`);
	} else if (why !== undefined) {
		console.log(`SKIP ${engine} ${version} cannot prove its route key: ${why}`);
		code = 77;
	} else {
		code = 0;
	}
} catch (err) {
	console.error(`FAIL ${engine} ${version}: ${err instanceof Error ? err.message : String(err)}`);
} finally {
	await finishTraces(code !== 0 && code !== 77);
	await browser.close().catch(() => {});
	server.stop();
}
process.exit(code);
