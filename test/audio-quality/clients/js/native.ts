/**
 * Plays one native row: the shipped `moq play --video off` through the shaper, into the private null
 * sink that `test/lib/pulse.py` runs, with `--metrics` writing what its speaker played.
 *
 * The player dials the shaper with the relay's certificate pinned by hash, so nothing on TCP answers
 * it there: the session is WebTransport through the UDP shaper, or the row is void.
 *
 *     bun native.ts --moq target/debug/moq --url https://127.0.0.1:4501 \
 *         --fingerprint http://127.0.0.1:4500/certificate.sha256 --broadcast tone-opus.hang \
 *         --delay auto --duration 60 --tag native-opus-48000-mild --out <run dir>
 *
 * Leaves the same pair the browser driver does: `<tag>.ndjson`, the player's own samples, and
 * `<tag>.page.json`, the environment, notes, and voids. The player's log is `player-<tag>.log`.
 * Exits nonzero when playback fails.
 *
 * @module
 */
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { parseArgs } from "node:util";
import { sleep } from "../../../interop/clients/js/harness.ts";
import type { Environment, Sample, Void } from "./src/schema.ts";

const { values } = parseArgs({
	options: {
		moq: { type: "string" },
		url: { type: "string" },
		fingerprint: { type: "string" },
		broadcast: { type: "string" },
		delay: { type: "string", default: "auto" },
		duration: { type: "string", default: "60" },
		tag: { type: "string" },
		out: { type: "string" },
	},
});

const { moq, url, fingerprint, broadcast, tag, out } = values;
const durationMs = Number.parseFloat(values.duration) * 1000;
if (!moq || !url || !fingerprint || !broadcast || !tag || !out || !Number.isFinite(durationMs) || durationMs <= 0) {
	console.error(
		"usage: native.ts --moq BIN --url U --fingerprint F --broadcast B --tag T --out DIR [--delay auto] [--duration S>0]",
	);
	process.exit(2);
}
if (!process.env.PULSE_SERVER) {
	console.error("error: run under test/lib/pulse.py, so the player opens the private null sink");
	process.exit(2);
}

/** How long the session and the first audio get, together: the browser driver's budget. */
const STARTUP_MS = 20_000;
/** How long the player gets to write its last sample and exit after Ctrl-C. */
const SHUTDOWN_MS = 10_000;

const samplesFile = join(out, `${tag}.ndjson`);
const logFile = join(out, `player-${tag}.log`);
const voids: Void[] = [];
const refuse = (assertion: string, detail: string) => {
	console.error(`void: ${assertion}: ${detail}`);
	voids.push({ assertion, detail });
};

const samples = (): Sample[] =>
	existsSync(samplesFile)
		? readFileSync(samplesFile, "utf8")
				.split("\n")
				.filter((line) => line.trim() !== "")
				.map((line) => JSON.parse(line) as Sample)
		: [];

// biome-ignore lint/suspicious/noControlCharactersInRegex: stripping the log's colour codes
const log = () => (existsSync(logFile) ? readFileSync(logFile, "utf8").replace(/\x1b\[[0-9;]*m/g, "") : "");

const hash = await fetch(fingerprint).then((r) => {
	if (!r.ok) throw new Error(`${fingerprint}: ${r.status}`);
	return r.text();
});

const args = [
	moq,
	"--connect",
	url,
	"--connect-tls-fingerprint",
	hash.trim(),
	"--broadcast",
	broadcast,
	"play",
	"--video",
	"off",
	"--metrics",
	samplesFile,
];
// The element takes "auto" or a duration, and so does the player.
if (values.delay !== "auto") args.push("--delay", values.delay);
console.log(`player: ${args.join(" ")}`);
// The player logs to stderr and writes nothing to stdout.
const player = Bun.spawn(args, {
	stdout: "inherit",
	stderr: Bun.file(logFile),
	env: { ...process.env, NO_COLOR: "1" },
});

let failed = false;
try {
	const deadline = Date.now() + STARTUP_MS;
	while (!samples().some((s) => s.timestamp !== undefined && s.stalled === false)) {
		if (player.exitCode !== null) throw new Error(`the player exited ${player.exitCode} before first audio`);
		if (Date.now() >= deadline) throw new Error(`first audio not within ${STARTUP_MS / 1000}s`);
		await sleep(250);
	}

	const end = Date.now() + durationMs;
	while (Date.now() < end) {
		if (player.exitCode !== null) throw new Error(`the player exited ${player.exitCode} mid-row`);
		await sleep(Math.min(1000, end - Date.now()));
	}

	player.kill("SIGINT");
	const exited = await Promise.race([player.exited, sleep(SHUTDOWN_MS).then(() => undefined)]);
	if (exited === undefined) throw new Error(`the player ignored Ctrl-C for ${SHUTDOWN_MS / 1000}s`);
	if (exited !== 0) throw new Error(`the player exited ${exited} on Ctrl-C; see ${logFile}`);
} catch (err) {
	failed = true;
	refuse("driver", err instanceof Error ? err.message : String(err));
} finally {
	if (player.exitCode === null) player.kill("SIGKILL");
	await player.exited;
}

const text = log();
const transport = text.match(/ connected .*transport=(\S+)/)?.[1];
const device = text.match(/opened audio output rate=(\d+)/)?.[1];
const environment: Environment = {
	transport,
	contextRate: device === undefined ? undefined : Number(device),
};
if (!failed && transport !== "webtransport") {
	refuse("transport", `negotiated ${transport ?? "nothing logged"}, which never crosses the UDP shaper`);
}
const notes = text
	.split("\n")
	.filter((line) => / (WARN|ERROR) /.test(line))
	.slice(0, 200)
	.map((line) => line.slice(0, 200));

await Bun.write(join(out, `${tag}.page.json`), JSON.stringify({ environment, notes, voids }, null, 1));
process.exit(failed ? 1 : 0);
