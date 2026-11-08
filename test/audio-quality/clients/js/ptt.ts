/**
 * Run 22 microphone presses through the real publisher, relay, and players: the watch element in
 * the same page, and the native `moq play` on its own session, writing `native.ndjson`.
 *
 * @module
 */
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { parseArgs } from "node:util";
import { createServer } from "vite";
import { worklet } from "../../../../js/common/vite-plugin-worklet";
import { launch, sleep } from "../../../interop/clients/js/harness";
import type { measurement } from "./src/ptt";
import type { Sample } from "./src/schema";

const { values } = parseArgs({
	options: {
		relay: { type: "string" },
		moq: { type: "string" },
		out: { type: "string" },
	},
});
const { relay, moq, out } = values;
if (!relay || !moq || !out) throw new Error("usage: ptt.ts --relay URL --moq BIN --out DIR");

/** How long the native player gets to reach its session and first audio: the audio lane's budget. */
const STARTUP_MS = 20_000;
/** How long it gets to write its last sample and exit after Ctrl-C. */
const SHUTDOWN_MS = 10_000;
const metrics = join(out, "native.ndjson");
const nativeLog = join(out, "native.log");
const samples = (): Sample[] =>
	existsSync(metrics)
		? readFileSync(metrics, "utf8")
				.split("\n")
				.filter((line) => line.trim() !== "")
				.map((line) => JSON.parse(line) as Sample)
		: [];

// Chromium loops this constant tone as its microphone. Its default fake device has silent
// intervals of its own, which would make an audible onset ambiguous.
const wav = Buffer.alloc(44 + 48_000 * 2);
wav.write("RIFF", 0);
wav.writeUInt32LE(wav.length - 8, 4);
wav.write("WAVEfmt ", 8);
wav.writeUInt32LE(16, 16);
wav.writeUInt16LE(1, 20);
wav.writeUInt16LE(1, 22);
wav.writeUInt32LE(48_000, 24);
wav.writeUInt32LE(96_000, 28);
wav.writeUInt16LE(2, 32);
wav.writeUInt16LE(16, 34);
wav.write("data", 36);
wav.writeUInt32LE(wav.length - 44, 40);
for (let i = 0; i < 48_000; i++)
	wav.writeInt16LE(Math.round(8192 * Math.sin((2 * Math.PI * 440 * i) / 48_000)), 44 + i * 2);
const tone = join(out, "tone.wav");
await Bun.write(tone, wav);

const server = await createServer({
	configFile: false,
	root: import.meta.dir,
	plugins: [worklet()],
	server: { host: "127.0.0.1", port: 0 },
});
await server.listen();
let native: ReturnType<typeof Bun.spawn> | undefined;
const browser = await launch([
	"--autoplay-policy=no-user-gesture-required",
	"--use-fake-device-for-media-stream",
	"--use-fake-ui-for-media-stream",
	`--use-file-for-fake-audio-capture=${tone}`,
]);
try {
	const page = await browser.newPage();
	const pageErrors: string[] = [];
	page.on("pageerror", (error) => pageErrors.push(String(error)));
	await page.goto(`${server.resolvedUrls?.local[0]}ptt.html?${new URLSearchParams({ relay })}`);
	await page.waitForFunction(() => globalThis.ptt?.state().selected && globalThis.ptt.state().tapped, undefined, {
		timeout: 30_000,
	});
	const initial = await page.evaluate(() => globalThis.ptt.state());
	if (initial.publisher !== "webtransport" || initial.viewer !== "webtransport") throw new Error("not WebTransport");
	if (!initial.separate) throw new Error("the watcher shares the publisher's origin, bypassing the relay");
	// The native listener joins while the publisher talks, as the page's did, at the same fixed
	// delay, on a private null sink. It dials the relay with its certificate pinned by hash, so
	// nothing on TCP answers it there: the session is WebTransport or it fails.
	const hash = await fetch(new URL("/certificate.sha256", relay)).then((r) => {
		if (!r.ok) throw new Error(`certificate.sha256: ${r.status}`);
		return r.text();
	});
	const dial = new URL(relay);
	dial.protocol = "https:";
	native = Bun.spawn(
		[
			"python3",
			join(import.meta.dir, "../../../lib/pulse.py"),
			"--dir",
			join(out, "pulse"),
			"--",
			moq,
			"--connect",
			dial.toString(),
			"--connect-tls-fingerprint",
			hash.trim(),
			"--broadcast",
			"ptt.hang",
			"play",
			"--video",
			"off",
			"--delay",
			"300ms",
			"--metrics",
			metrics,
		],
		{ stdout: "inherit", stderr: Bun.file(nativeLog), env: { ...process.env, NO_COLOR: "1" } },
	);
	const deadline = Date.now() + STARTUP_MS;
	while (!samples().some((s) => s.stalled === false)) {
		if (native.exitCode !== null) throw new Error(`the native player exited ${native.exitCode} before first audio`);
		if (Date.now() >= deadline) throw new Error(`no native audio within ${STARTUP_MS / 1000}s`);
		await sleep(250);
	}
	await page.waitForTimeout(3000);
	await page.evaluate(() => globalThis.ptt.press(false));
	await page.waitForTimeout(5000);
	const held = await page.evaluate(() => globalThis.ptt.state());
	if (held.active || !held.config || held.config.enabled === false || held.selected !== initial.selected) {
		throw new Error("muting changed the rendition or the listener's selection");
	}
	// Tune-in can consume retained audio. Validate the tap on a fresh press only after the muted
	// listener keeps its subscription.
	await page.evaluate(() => globalThis.ptt.press(true));
	await page.waitForTimeout(3000);
	await page.evaluate(() => globalThis.ptt.press(false));
	await page.waitForTimeout(5000);
	const warmup = await page.evaluate(() => globalThis.ptt.result());
	await Bun.write(join(out, "warmup.json"), JSON.stringify({ browser: browser.version(), initial, held, ...warmup }));
	if (!warmup.audible.length) throw new Error("the tone never reached the output tap");
	await page.evaluate(() => globalThis.ptt.reset());
	for (let i = 0; i < 22; i++) {
		if (native.exitCode !== null) throw new Error(`the native player exited ${native.exitCode} mid-run`);
		await page.waitForTimeout(i < 8 ? 0 : i < 15 ? 5000 : 60_000);
		await page.evaluate(() => globalThis.ptt.press(true));
		await page.waitForTimeout(i % 2 === 0 ? 2000 : 3000);
		await page.evaluate(() => globalThis.ptt.press(false));
		console.log(`press ${i + 1}/22`);
		await Bun.write(join(out, "presses.json"), JSON.stringify(await page.evaluate(() => globalThis.ptt.result())));
	}
	await page.waitForTimeout(5000);
	const result: ReturnType<typeof measurement.result> = await page.evaluate(() => globalThis.ptt.result());
	const state = await page.evaluate(() => globalThis.ptt.state());
	await Bun.write(
		join(out, "presses.json"),
		JSON.stringify({ browser: browser.version(), ...result, state, pageErrors }),
	);
	if (pageErrors.length || result.errors.length || result.captureError || state.error)
		throw new Error("measurement failed; see presses.json");
	// Ctrl-C closes out the last gap and writes a final sample.
	native.kill("SIGINT");
	const exited = await Promise.race([native.exited, sleep(SHUTDOWN_MS).then(() => undefined)]);
	if (exited === undefined) throw new Error(`the native player ignored Ctrl-C for ${SHUTDOWN_MS / 1000}s`);
	if (exited !== 0) throw new Error(`the native player exited ${exited} on Ctrl-C; see ${nativeLog}`);
	const transport = readFileSync(nativeLog, "utf8").match(/ connected .*transport=(\S+)/)?.[1];
	if (transport !== "webtransport") throw new Error(`the native player negotiated ${transport ?? "nothing logged"}`);
} finally {
	// The wrapper forwards it, so the player goes before its audio server.
	if (native && native.exitCode === null) native.kill("SIGTERM");
	await native?.exited;
	await browser.close();
	await server.close();
}
