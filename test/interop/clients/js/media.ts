/**
 * Browser media QA: does a viewer actually get advancing, synchronized media, and does the player
 * recover from the lifecycle changes an application makes?
 *
 * The interop matrix (`driver.ts`) answers "did bytes arrive and did a pixel light up". That passes
 * on a frozen picture, on silence, and on audio a second out of step. This drives the deterministic
 * fixture instead (`src/fixture.ts`), whose media says what it is, and measures both sinks: the
 * frame counter painted on the canvas and the tone step on the audio graph. Everything asserted
 * here is browser output. Nothing here says anything about what a physical speaker emits.
 *
 * Launches with Chromium's gesture-required autoplay policy. The capture case adds a fake device
 * while Playwright controls permission. No page reloads: the publisher reports when it is ready.
 * `--publisher-engine` and `--player-engine` pick each role's engine, so a Firefox publisher takes
 * the capture path Chromium never does.
 *
 *     bun media.ts --url http://127.0.0.1:4443 [--timeout 30] [--cases pause,detach]
 *     bun media.ts --url ... --publisher-engine firefox --player-engine chromium
 *     bun media.ts --url ... --fault silent-audio --cases none --expect-fail "audio tone"
 *     bun media.ts --url ... --lag --cases late-join --expect-fail "late join reaches live"
 *     bun media.ts --url ... --publisher native --broadcast B [--cases audio-switch]
 *
 * `--publisher native` watches a broadcast someone else publishes with the same pattern (interop.sh
 * runs ffmpeg into the Rust CLI, see `src/native.ts`) instead of opening the fixture, so only the
 * cases that need nothing from the publisher's page run.
 *
 * The AAC cases need a publisher that can encode AAC, which Chromium does only through a platform
 * encoder. Where it has none, selecting them exits {@link SKIPPED} after the capability probe.
 *
 * @module
 */
import { parseArgs } from "node:util";
import type MoqWatch from "@moq/watch/element";
import type { Browser, Page } from "playwright";
import {
	type BrowserErrors,
	check,
	command,
	ENGINES,
	type Engine,
	Failure,
	finishTraces,
	launch,
	open,
	POLL_INTERVAL_MS,
	pageUrl,
	pause,
	readCaptureState,
	readFixtureState,
	readPlayerState,
	SELECTORS,
	serve,
	sleep,
	startTrace,
	throwPageErrors,
	waitFor,
	waitForFixture,
	waitForResources,
	waitForState,
	waitForWatch,
} from "./harness";
import { type Capabilities, capturePath, missing, probe, type Role } from "./src/capabilities";
import {
	ALT_AUDIO,
	AUDIBLE_RMS,
	CHANGED_SAMPLE_RATE,
	FAULTS,
	KEYFRAME_INTERVAL_MS,
	LONG_FRAMES,
	type LongFrame,
	leakedPlayerStarted,
	reachesLive,
	SAMPLE_MS,
	SAMPLE_RATE,
	type Sample,
} from "./src/contract";
import * as Pattern from "./src/pattern";
import { resumeSkew } from "./src/resume";

/** Cases beyond the mandatory capability probe, publisher readiness, and cold start. */
const CASES = [
	"capture-denial",
	"pause",
	"disable",
	"rejoin",
	"detach",
	"republish",
	"replacement",
	"late-join",
	"rename",
	"audio-format",
	"audio-switch",
	"mute-volume",
] as const;

/** Cases that need the publisher to encode AAC, run only when selected. */
const AAC_CASES = ["aac", "audio-codec"] as const;

/** Cases a native publisher supports: those that drive only the player. */
const NATIVE_CASES = ["audio-switch"] as const;

type Case = (typeof CASES)[number] | (typeof AAC_CASES)[number];
const KNOWN: readonly Case[] = [...CASES, ...AAC_CASES];

/** The exit code of a run whose selected cases this host cannot run, which interop.sh records as a skip. */
const SKIPPED = 77;

const { values } = parseArgs({
	options: {
		url: { type: "string" },
		"publisher-engine": { type: "string", default: "chromium" },
		"player-engine": { type: "string", default: "chromium" },
		publisher: { type: "string", default: "fixture" },
		broadcast: { type: "string" },
		timeout: { type: "string", default: "30" },
		fault: { type: "string", default: "none" },
		cases: { type: "string" },
		focused: { type: "boolean", default: false },
		leak: { type: "boolean", default: false },
		lag: { type: "boolean", default: false },
		"expect-fail": { type: "string" },
	},
});

const url = values.url;
const timeoutMs = Number.parseFloat(values.timeout ?? "30") * 1000;
const fault = values.fault ?? "none";
const expectFail = values["expect-fail"];
const native = values.publisher === "native";
const isEngine = (name: string): name is Engine => ENGINES.some((engine) => engine === name);
const requestedPublisher = values["publisher-engine"] ?? "chromium";
const requestedPlayer = values["player-engine"] ?? "chromium";
const selected = new Set<string>(
	values.cases === undefined
		? native
			? NATIVE_CASES
			: CASES
		: values.cases === "none"
			? []
			: values.cases.split(","),
);
const unknown = [...selected].filter((name) => !(native ? NATIVE_CASES : KNOWN).some((c: string) => c === name));
if (
	!url ||
	!Number.isFinite(timeoutMs) ||
	timeoutMs <= 0 ||
	!FAULTS.some((f) => f === fault) ||
	unknown.length > 0 ||
	!isEngine(requestedPublisher) ||
	!isEngine(requestedPlayer) ||
	(values.focused && (values.cases === undefined || selected.size === 0)) ||
	(values.publisher !== "fixture" && !native) ||
	native !== (values.broadcast !== undefined) ||
	(native && (fault !== "none" || values.leak || values.lag || requestedPublisher !== "chromium"))
) {
	console.error(
		`usage: media.ts --url U [--publisher-engine ${ENGINES.join("|")}] [--player-engine ${ENGINES.join("|")}] [--timeout S>0] [--fault ${FAULTS.join("|")}] [--cases none|${KNOWN.join(",")}] [--focused] [--leak] [--lag] [--expect-fail TEXT]\n` +
			`       media.ts --url U --publisher native --broadcast B [--player-engine ${ENGINES.join("|")}] [--cases none|${NATIVE_CASES.join(",")}] [--expect-fail TEXT]`,
	);
	process.exit(2);
}
const publisherEngine = requestedPublisher as Engine;
const playerEngine = requestedPlayer as Engine;
const wants = (name: Case) => selected.has(name);

// process.exit narrows `url` above, but not inside the function declarations below.
const relay: string = url;

/**
 * The relay URL a page in `engine` dials. Chromium keeps normal transport selection; the others
 * explicitly use WebSocket, so a run never measures their QUIC paths.
 */
function relayFor(engine: Engine): string {
	const address = new URL(relay);
	if (engine !== "chromium") address.protocol = address.protocol === "https:" ? "wss:" : "ws:";
	return address.toString();
}

/** What `@moq/publish` logs when it captures through a `<video>` element; see its `processor.ts`. */
const CAPTURE_FALLBACK_WARNING = "MediaStreamTrackProcessor polyfill";

/** How long each media measurement window runs. Long enough to cover several tone cycles. */
const WINDOW_MS = 4000;

/** How long a state change (pause, unsubscribe, republish) has to take effect. */
const SETTLE_MS = 6000;

/** The replacement case's fixed player delay, so the switch bound below can count it. */
const REPLACEMENT_DELAY_MS = 50;

/**
 * How long a following player may take to present a replacement or fallback once the new route is
 * ready or the old one withdrawn. Routing: the relay holds a route change for its update hold
 * (moq-net's `origin::DEFAULT_UPDATE_HOLD`; the JS origin holds none), and the announcement plus
 * the catalog and track subscriptions take five loopback round trips, each charged at QUIC's 1 ms
 * timer granularity. Presentation: a keyframe interval of codec cadence, one frame to decode, and
 * the player's delay. Observation: one page sample and one driver poll.
 */
const SWITCH_MS = Math.ceil(
	300 + 5 + KEYFRAME_INTERVAL_MS + 1000 / Pattern.FPS + REPLACEMENT_DELAY_MS + SAMPLE_MS + POLL_INTERVAL_MS,
);

/** Fraction of a window's samples that must be readable, carry a tone, and be in sync. */
const AGREEMENT = 0.9;

/**
 * Presented frames per second a window must sustain, as a fraction of the fixture's rate.
 *
 * Well under 1: the probe samples asynchronously, the encoder drops frames under load, and a
 * headless run shares a CPU with the publisher. The bar is "the picture is advancing at roughly
 * the source rate", not "every frame arrived".
 */
const MIN_RATE = 0.5;

/**
 * Tolerated audio/video skew, in tone steps, so 200ms per step either way.
 *
 * The analyser's window straddles a step boundary for about a fifth of every step, and the canvas
 * holds the frame painted up to one frame ago, so a perfectly synchronized stream still reads one
 * step off some of the time. Two steps is not boundary noise.
 */
const MAX_SKEW_STEPS = 1;

/** Whether a sample plays the picture's own tone: audible, and within the tolerated skew of the frame it shows. */
const playsInSync = (s: Sample) =>
	s.frameId !== undefined &&
	s.toneStep !== undefined &&
	Math.abs(Pattern.stepSkew(s.toneStep, Pattern.expectedStep(s.frameId))) <= MAX_SKEW_STEPS;

const percentile = (values: number[], p: number) => {
	if (values.length === 0) return Number.NaN;
	const sorted = [...values].sort((a, b) => a - b);
	return sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * p))];
};

/** Collect distinct samples from the player for `ms`, so a window can be measured after the fact. */
async function collect(page: Page, errors: BrowserErrors, ms: number): Promise<Sample[]> {
	const samples: Sample[] = [];
	const deadline = Date.now() + ms;
	while (Date.now() < deadline) {
		throwPageErrors(errors);
		const sample = await readPlayerState(page).catch(() => undefined);
		if (sample && sample.seq !== samples[samples.length - 1]?.seq) samples.push(sample);
		await sleep(SAMPLE_MS / 2);
	}
	throwPageErrors(errors);
	return samples;
}

/** How long the presented frame must hold still to count as stopped. */
const FROZEN_MS = 1000;

/** How long the picture is watched after a pause settles, to prove it stays on the one frame. */
const HELD_MS = 1500;

/**
 * Wait until the presented frame stops moving, and return the frame it stopped on.
 *
 * The picture, not a status flag, is what a viewer sees stop, so this is what "playback stopped"
 * has to mean. A canvas that goes unreadable counts as stopped too.
 */
async function waitFrozen(page: Page, errors: BrowserErrors, assertion: string, description: string): Promise<number> {
	const deadline = Date.now() + SETTLE_MS;
	let frame: number | undefined;
	let since = Date.now();
	while (Date.now() < deadline) {
		throwPageErrors(errors);
		const current = (await readPlayerState(page).catch(() => undefined))?.frameId;
		if (current !== frame) {
			frame = current;
			since = Date.now();
		} else if (Date.now() - since >= FROZEN_MS) {
			return frame ?? 0;
		}
		await sleep(POLL_INTERVAL_MS);
	}
	throwPageErrors(errors);
	throw new Failure(assertion, `${description}: the presented frame is still advancing, now ${frame}`);
}

/**
 * A presented frame at most this far behind the fixture's painted counter counts as live.
 *
 * The lag spans capture, encode, relay, playout delay, and decode. Loaded runs settle at 4 to 24
 * frames with spikes to 33, while a player stuck on the GOP from before a demand gap sits 60 or more
 * behind.
 */
const LIVE_LAG_FRAMES = 45;

/** How long a latecomer may take to present its first frame, from its page loading. Loaded runs take up to 2.6s. */
const LATE_START_MS = 5000;

/** How long a latecomer may take to reach live after its first frame. Loaded runs take up to 0.3s. */
const LATE_CATCHUP_MS = 2000;

/** A player delay far enough behind {@link LIVE_LAG_FRAMES} that `--lag` must fail "late join reaches live". */
const LAGGED_DELAY = "3s";

const framesToMs = (frames: number) => Math.round((frames * 1000) / Pattern.FPS);

/**
 * Wait until the player is playing within {@link LIVE_LAG_FRAMES} of the frame the fixture last
 * painted, and report how far behind its first reading was.
 *
 * Both pages are read at once, in separate browsers, so the lag is what a viewer would see against
 * the source. After a demand gap the relay may hand over the GOP from before the gap, so the first
 * frame can be stale; `assertion` names the check that playback then fast-forwards to live.
 *
 * Only a frame that moved since the last reading counts. The player paints the first frame it decodes
 * as a preview and holds it until its sync delay comes up, so a held frame says nothing about where
 * playback runs: a player delayed seconds behind live still shows a near-live preview first.
 * Audio must also have left preroll, just as in measurePlayback: a new preview can replace an old
 * one while the audio buffer is still filling, so a changed canvas alone does not prove playback.
 *
 * A `resumed` player gets the full timeout to start moving again, and the catch-up window opens at
 * its first advancing frame. Every audible advancing pair before live is graded for A/V sync.
 */
async function waitForLive(
	publisher: Page,
	publisherErrors: BrowserErrors,
	player: Page,
	playerErrors: BrowserErrors,
	assertion: string,
	resumed = false,
): Promise<{ first: number; lag: number; after: number }> {
	let since = Date.now();
	let started = !resumed;
	const startup = since + timeoutMs;
	let first: number | undefined;
	let lag: number | undefined;
	let frame: number | undefined;
	let moved = false;
	// Live needs audio playing too, so a failure says whether audio or the picture held it back.
	let audio = "unread";
	while (Date.now() < (started ? since + LATE_CATCHUP_MS : startup)) {
		throwPageErrors(publisherErrors);
		throwPageErrors(playerErrors);
		const [fixture, state] = await Promise.all([readFixtureState(publisher), readPlayerState(player)]).catch(
			() => [],
		);
		if (fixture && state?.frameId !== undefined) {
			if (resumed) {
				const skew = resumeSkew(state, frame);
				if (skew !== undefined)
					check(
						Math.abs(skew) <= MAX_SKEW_STEPS * Pattern.STEP_MS,
						"resume audio/video sync",
						() => `${assertion}: offset ${skew}ms; ${traceLine(state, since)}`,
					);
			}
			lag = fixture.frameId - state.frameId;
			first ??= lag;
			audio =
				state.audioContext === "running" && !state.audioStalled
					? "playing"
					: `${state.audioContext ?? "not built"}${state.audioStalled ? " and buffering" : ""}`;
			moved = frame !== undefined && state.frameId !== frame;
			if (moved && !started) {
				started = true;
				since = Date.now();
			}
			const live = reachesLive({
				previous: frame,
				source: fixture.frameId,
				sample: state,
				bound: LIVE_LAG_FRAMES,
			});
			frame = state.frameId;
			if (live) return { first, lag, after: Date.now() - since };
		}
		await sleep(POLL_INTERVAL_MS);
	}
	throwPageErrors(publisherErrors);
	throwPageErrors(playerErrors);
	throw new Failure(
		assertion,
		`${moved ? "still" : "held a frame"} ${lag === undefined ? "?" : framesToMs(lag)}ms behind the fixture ` +
			`${started ? LATE_CATCHUP_MS : timeoutMs}ms after the first frame, against a ${framesToMs(LIVE_LAG_FRAMES)}ms bound for playback; audio ${audio}`,
	);
}

/**
 * Measure one window of playback, opened once the player has filled its audio buffer and plays.
 *
 * A player shows its first frame as soon as it decodes, then holds that frame silent while it buffers
 * up to its sync delay, which rises past 400ms under load. A window opened on that held frame counts
 * the buffering as missing tone. Only the ring filling opens the window, never a tone, so silence
 * still fails {@link assertMedia} rather than this wait.
 */
async function measurePlayback(player: Page, playerErrors: BrowserErrors, label: string): Promise<void> {
	await waitForState(player, playerErrors, {
		deadline: Date.now() + SETTLE_MS,
		assertion: "playback starts",
		description: `${label}: the audio buffer to fill to the player's delay and play`,
		predicate: (state) => state.audioContext === "running" && !state.audioStalled,
	});
	await (await observe(player, playerErrors, label)).grade();
}

/**
 * Collect one window while the host is watched, so a window that fails `grade`, whether on a
 * caller's own transition checks or on {@link assertMedia}, also prints what stalled the pages.
 */
async function observe(
	player: Page,
	playerErrors: BrowserErrors,
	label: string,
): Promise<{ samples: Sample[]; grade: (checks?: () => void) => Promise<void> }> {
	const host = watchHost(label);
	const start = Date.now();
	const samples = await collect(player, playerErrors, WINDOW_MS);
	const bounds = { start, end: Date.now() };
	const grade = async (checks?: () => void) => {
		try {
			checks?.();
			assertMedia(samples, label);
		} catch (err) {
			await explainStalls(player, bounds, label, host);
			throw err;
		} finally {
			host?.process.kill();
		}
	};
	return { samples, grade };
}

/**
 * How long a failed window waits for each page to report its long animation frames. Playwright puts
 * no timeout on an evaluate, so a page whose main thread never returns would otherwise hold the
 * failure, the other page's evidence, the host's and the browser cleanup until the job times out.
 */
const STALL_READ_MS = 2000;

/** How often the host's busiest processes are read during a window. */
const HOST_INTERVAL_S = 0.5;

/**
 * Read the host's busiest processes for the length of one window, on Linux, where `top` has a batch
 * mode. A page's main thread stopping for hundreds of milliseconds is either the page's own work or a
 * starved host, and only the host can say which.
 */
function watchHost(
	label: string,
): { process: Bun.Subprocess<"ignore", "pipe", "ignore">; output: Promise<string> } | undefined {
	if (process.platform !== "linux") return undefined;
	const iterations = Math.ceil(WINDOW_MS / 1000 / HOST_INTERVAL_S) + 1;
	// Evidence only: a host without `top` loses this part of a failure's explanation, not the window.
	try {
		const top = Bun.spawn(
			["top", "-b", "-c", "-w", "512", "-d", String(HOST_INTERVAL_S), "-n", String(iterations)],
			{ stdin: "ignore", stdout: "pipe", stderr: "ignore" },
		);
		return { process: top, output: new Response(top.stdout).text() };
	} catch (err) {
		console.error(`  ${label}: host processes unavailable: ${err}`);
		return undefined;
	}
}

// A process as `top -c` prints it, cut to what tells the processes apart: the binary, plus the type of
// a Chromium child (renderer, gpu-process, utility), whose binaries all share one name.
function processName(command: string): string {
	const [binary = "", ...args] = command.split(/\s+/);
	const type = args.find((arg) => arg.startsWith("--type="))?.slice("--type=".length);
	const name = binary.split("/").pop() ?? binary;
	return type ? `${name}:${type}` : name;
}

/**
 * Print what stopped the pages' main threads during a failed window: each page's long animation
 * frames with the scripts that ran in them, and the host's busiest processes.
 *
 * Evidence only. A page that cannot be read leaves its part out rather than masking the failure.
 */
async function explainStalls(
	player: Page,
	bounds: { start: number; end: number },
	label: string,
	host: ReturnType<typeof watchHost>,
): Promise<void> {
	// The window has already failed, so a plain evaluate's user activation can no longer skew it, and
	// unlike `inspect` it reads a non-Chromium player too. The bounds come from the driver's clock,
	// which the pages' epoch timestamps share, so each page reports on its own and one that cannot be
	// read, or does not answer within STALL_READ_MS, leaves the other's evidence intact.
	const { start, end } = bounds;
	for (const [name, page] of [
		["player", player],
		["fixture", fixturePage],
	] as const) {
		if (!page) continue;
		let timer: ReturnType<typeof setTimeout> | undefined;
		try {
			const unanswered = new Promise<never>((_, reject) => {
				timer = setTimeout(
					() => reject(new Error(`no answer within ${STALL_READ_MS}ms, so its main thread is still blocked`)),
					STALL_READ_MS,
				);
			});
			const frames = await Promise.race([
				page.evaluate(
					({ key, from, to }) =>
						((globalThis as unknown as Record<string, LongFrame[] | undefined>)[key] ?? []).filter(
							(frame) => frame.at + frame.duration >= from && frame.at <= to,
						),
					{ key: LONG_FRAMES, from: start, to: end },
				),
				unanswered,
			]);
			console.error(`  ${label}: ${name} long animation frames: ${frames.length}`);
			for (const frame of frames) {
				console.error(
					`    +${((frame.at - start) / 1000).toFixed(2)}s ${frame.duration}ms (blocking ${frame.blocking}ms) ${frame.scripts}`,
				);
			}
		} catch (err) {
			console.error(`  ${label}: ${name} long animation frames unavailable: ${err}`);
		} finally {
			clearTimeout(timer);
		}
	}

	if (!host) return;
	host.process.kill();
	const snapshots = (await host.output).split(/^top - /m).slice(1);
	console.error(`  ${label}: busiest host processes every ${HOST_INTERVAL_S}s from the window's start`);
	for (const [index, snapshot] of snapshots.entries()) {
		const lines = snapshot.split("\n");
		const idle = lines.find((line) => line.startsWith("%Cpu"))?.match(/([\d.]+) id/)?.[1];
		const header = lines.findIndex((line) => /^\s*PID\s/.test(line));
		const busiest = lines
			.slice(header + 1, header + 7)
			.map((line) => line.trim().split(/\s+/))
			.filter((fields) => fields.length >= 12)
			.map((fields) => `${processName(fields.slice(11).join(" "))} ${fields[8]}%`);
		console.error(`    +${(index * HOST_INTERVAL_S).toFixed(1)}s idle ${idle ?? "?"}%: ${busiest.join(", ")}`);
	}
}

// One sample as a line: elapsed time, the frame on the canvas, the tone step heard against the one
// that frame belongs to, how far the tone stood above the noise floor, and whether audio was
// arriving at all, which separates a silent player from a publisher that stopped sending.
function traceLine(sample: Sample, start: number): string {
	const step = sample.frameId === undefined ? "?" : Pattern.expectedStep(sample.frameId);
	const margin = sample.toneDb !== undefined && sample.noiseDb !== undefined ? sample.toneDb - sample.noiseDb : 0;
	return (
		`    +${((sample.at - start) / 1000).toFixed(2)}s frame=${sample.frameId ?? "-"} ` +
		`step=${sample.toneStep ?? "-"}/${step} tone=${margin.toFixed(0)}dB ${sample.toneHz?.toFixed(0) ?? "-"}Hz ` +
		`rms=${sample.rms?.toFixed(3) ?? "-"} ats=${sample.audioTimestamp?.toFixed(0) ?? "-"} vts=${sample.videoTimestamp?.toFixed(0) ?? "-"} ` +
		`generation=${sample.generation ?? "-"} track=${sample.videoTrack ?? "-"} codec=${sample.videoCodec ?? "-"} ` +
		`paused=${sample.paused} delay=${sample.delay}ms audio=${sample.audioBytes}B${sample.audioStalled ? " stalled" : ""} ` +
		`${sample.audioTrack ?? "-"}:${sample.audioCodec ?? "-"}/${sample.audioSampleRate ?? "-"}x${sample.audioChannels ?? "-"} speaker=${sample.speakerRms?.toFixed(3) ?? "-"}`
	);
}

/**
 * Assert the window shows advancing, audible, synchronized media, and report what it measured.
 *
 * Every number here is read at a sink: the frame counter off the canvas the renderer paints, and
 * the tone off the graph root that feeds the speakers. A failure prints every reading in the window,
 * so the summary can be traced back to the readings behind it.
 */
function assertMedia(samples: Sample[], label: string): void {
	try {
		measure(samples, label);
	} catch (err) {
		const start = samples[0]?.at ?? 0;
		console.error(`  ${label}: ${samples.length} samples`);
		for (const sample of samples) console.error(traceLine(sample, start));
		throw err;
	}
}

function measure(samples: Sample[], label: string): void {
	check(samples.length >= 10, "sampling", () => `${label}: only ${samples.length} samples in the window`);

	const readable = samples.filter((s) => s.frameId !== undefined);
	check(
		readable.length >= samples.length * AGREEMENT,
		"pattern readable",
		() => `${label}: ${readable.length}/${samples.length} presented frames carried a readable fixture pattern`,
	);

	const first = readable[0];
	const last = readable[readable.length - 1];
	const elapsed = (last.at - first.at) / 1000;
	const advance = (last.frameId ?? 0) - (first.frameId ?? 0);
	const rate = advance / elapsed;
	const want = Pattern.FPS * MIN_RATE;
	check(
		rate >= want,
		"video progress",
		() =>
			`${label}: presented ${advance} frames in ${elapsed.toFixed(1)}s, ${rate.toFixed(1)}fps against a ${want.toFixed(1)}fps floor`,
	);

	const back = readable.findIndex((s, i) => i > 0 && (s.frameId ?? 0) < (readable[i - 1].frameId ?? 0));
	check(
		back < 0,
		"video monotonic",
		() =>
			`${label}: presented frame went backwards, ${readable[back - 1]?.frameId} then ${readable[back]?.frameId}`,
	);

	const toned = samples.filter((s) => s.toneStep !== undefined);
	check(
		toned.length >= samples.length * AGREEMENT,
		"audio tone",
		() => `${label}: ${toned.length}/${samples.length} samples carried the fixture tone above the noise floor`,
	);

	const skews = samples
		.filter((s) => s.frameId !== undefined && s.toneStep !== undefined)
		.map((s) => Pattern.stepSkew(s.toneStep as number, Pattern.expectedStep(s.frameId as number)));
	check(skews.length > 0, "audio/video sync", () => `${label}: no sample carried both a frame and a tone`);

	const aligned = skews.filter((skew) => Math.abs(skew) <= MAX_SKEW_STEPS);
	const median = percentile(skews.map(Math.abs), 0.5) * Pattern.STEP_MS;
	const p95 = percentile(
		skews.map((s) => Math.abs(s) * Pattern.STEP_MS),
		0.95,
	);
	check(
		aligned.length >= skews.length * AGREEMENT && median <= MAX_SKEW_STEPS * Pattern.STEP_MS,
		"audio/video sync",
		() =>
			`${label}: ${aligned.length}/${skews.length} samples within ${MAX_SKEW_STEPS * Pattern.STEP_MS}ms, median skew ${median.toFixed(0)}ms`,
	);

	const margin = percentile(
		toned.map((s) => (s.toneDb ?? 0) - (s.noiseDb ?? 0)),
		0.5,
	);
	console.error(
		`  ${label}: ${rate.toFixed(1)}fps presented over ${advance} frames, tone ${margin.toFixed(0)}dB above the floor, ` +
			`skew median ${median.toFixed(0)}ms p95 ${p95.toFixed(0)}ms (browser output, not a speaker)`,
	);
}

/**
 * Change the audio a player is receiving and grade one window across the change: the player has to
 * follow onto `arrived` and stay there, with the window's tone and sync held to the same agreement
 * as any other. A format change mints a new rendition track, so following it at all is the check,
 * not how it switches. The longest gap is reported.
 */
async function followAudio(
	player: Page,
	playerErrors: BrowserErrors,
	label: string,
	change: () => Promise<void>,
	arrived: (sample: Sample, before: Sample) => boolean,
): Promise<void> {
	console.error(`=== ${label} ===`);
	const before = await readPlayerState(player);
	await change();
	const window = await observe(player, playerErrors, label);
	const samples = window.samples;
	const first = samples.findIndex((sample) => arrived(sample, before));
	await window.grade(() => {
		try {
			followed(samples, first, before, arrived, label);
		} catch (err) {
			// assertMedia prints the window only when it fails itself.
			console.error(`  ${label}: ${samples.length} samples`);
			for (const sample of samples) console.error(traceLine(sample, samples[0]?.at ?? 0));
			throw err;
		}
	});
	const now = samples[samples.length - 1];
	// Reported rather than gated: today's player rebuffers across a re-minted or swapped track, so
	// the window above grades only the agreement the cold start is held to.
	console.error(
		`  ${label}: ${before.audioRendition}:${before.audioTrack} ${before.audioCodec} ${before.audioSampleRate}Hz x${before.audioChannels} ` +
			`-> ${now.audioRendition}:${now.audioTrack} ${now.audioCodec} ${now.audioSampleRate}Hz x${now.audioChannels}, ` +
			`longest audio gap ${longestGap(samples)}ms`,
	);
}

// The checks behind followAudio, over the window it collected.
function followed(
	samples: Sample[],
	first: number,
	before: Sample,
	arrived: (sample: Sample, before: Sample) => boolean,
	label: string,
): void {
	check(
		first >= 0,
		label,
		() =>
			`the player never followed from ${before.audioRendition}:${before.audioTrack} ` +
			`(${before.audioCodec} ${before.audioSampleRate}Hz x${before.audioChannels}): ${JSON.stringify(samples.at(-1))}`,
	);
	check(
		samples.slice(first).every((sample) => arrived(sample, before)),
		label,
		() => "the player went back to the audio it had before the change",
	);
}

/**
 * The longest stretch of a window the player spent without the tone, in milliseconds: from the first
 * sample that lost it, stalled or silent, to the next that carried it again.
 */
function longestGap(samples: Sample[]): number {
	let longest = 0;
	let since: number | undefined;
	for (const sample of samples) {
		const playing = sample.toneStep !== undefined && !sample.audioStalled;
		if (!playing) since ??= sample.at;
		else if (since !== undefined) {
			longest = Math.max(longest, sample.at - since);
			since = undefined;
		}
	}
	if (since !== undefined) longest = Math.max(longest, (samples.at(-1)?.at ?? since) - since);
	return Math.round(longest);
}

/** The audio renditions the player's catalog offers. Evaluates, so only after the player's gesture. */
function audioRenditions(player: Page): Promise<string[]> {
	return player
		.locator(SELECTORS.watch)
		.evaluate((el: MoqWatch) => Object.keys(el.broadcast.out.catalog.peek()?.audio?.renditions ?? {}));
}

/** Pin the player to an audio rendition by name, or back to its own choice. */
function targetAudio(player: Page, name: string | undefined): Promise<void> {
	return player
		.locator(SELECTORS.watch)
		.evaluate(
			(el: MoqWatch, name) => el.controls.audio.target.set(name === undefined ? undefined : { name }),
			name,
		);
}

/**
 * How far the speaker level may stray from the graph root's level times the volume.
 *
 * Both analysers sit in one context and are read in one tick, so they hold the same samples up to
 * the render quantum (128 frames, ~3ms) the two reads can straddle, against a 2048-frame window.
 */
const VOLUME_TOLERANCE = 0.1;

/** How long a volume change takes to land: the emitter's 200ms ramp, then a full analyser window. */
const VOLUME_SETTLE_MS = 200 + 50;

/**
 * Set the player's volume and check the speakers carry the graph root's level scaled by it.
 *
 * The probe's tone analyser hangs off the graph root, upstream of the emitter's private gain, so it
 * cannot see volume. The speaker tap (`src/instrument.ts`) sums what reaches the destination.
 */
async function checkVolume(player: Page, playerErrors: BrowserErrors, volume: number): Promise<void> {
	await player.locator(SELECTORS.watch).evaluate((el: MoqWatch, volume) => {
		el.volume = volume;
	}, volume);
	await sleep(VOLUME_SETTLE_MS);
	const samples = await collect(player, playerErrors, HELD_MS);
	const ratios = samples
		.filter((s) => s.rms !== undefined && s.rms >= AUDIBLE_RMS && s.speakerRms !== undefined && !s.audioStalled)
		.map((s) => (s.speakerRms as number) / (s.rms as number));
	check(
		ratios.length >= samples.length * AGREEMENT,
		"volume scales the output",
		() => `volume ${volume}: only ${ratios.length}/${samples.length} samples carried audio at the graph root`,
	);
	const median = percentile(ratios, 0.5);
	const off = ratios.filter((ratio) => Math.abs(ratio - volume) > VOLUME_TOLERANCE * Math.max(volume, 0.1));
	check(
		off.length <= ratios.length * (1 - AGREEMENT) && Math.abs(median - volume) <= VOLUME_TOLERANCE * volume,
		"volume scales the output",
		() =>
			`volume ${volume}: the speakers carried ${median.toFixed(3)} of the graph root's level (median), ` +
			`${off.length}/${ratios.length} samples outside ±${VOLUME_TOLERANCE * 100}%`,
	);
	console.error(`  volume ${volume}: speakers at ${median.toFixed(3)} of the graph root's level`);
}

// The capability page has its own context. Evaluation may grant activation there,
// but cannot unlock the measured fixture or player's audio graph.
const observed = new Map<Engine, { version: string; found: Capabilities }>();
async function capabilities(browser: Browser, engine: Engine, role: Role): Promise<void> {
	let record = observed.get(engine);
	if (!record) {
		const page = await browser.newPage();
		try {
			await page.route("**/*", (route) =>
				route.fulfill({ contentType: "text/html", body: "<!doctype html><title>Capabilities</title>" }),
			);
			await page.goto(server.origin);
			record = {
				version: browser.version(),
				found: await page.evaluate(probe, {
					sampleRate: SAMPLE_RATE,
					width: Pattern.WIDTH,
					height: Pattern.HEIGHT,
				}),
			};
			observed.set(engine, record);
		} finally {
			await page.close();
		}
		console.error(`=== engine capabilities: ${engine} ${record.version} ===`);
		for (const [name, ok] of Object.entries(record.found)) console.error(`  ${ok ? "yes" : "NO "} ${name}`);
		console.error(
			`  engine matrix: ${JSON.stringify({ engine, version: record.version, publisherMissing: missing(record.found, "publisher"), playerMissing: missing(record.found, "player") })}`,
		);
		if (engine === "webkit")
			console.error(
				"  Playwright WebKit is not Safari; Safari transport and device defects still need a manual run",
			);
	}
	const absent = missing(record.found, role);
	check(
		absent.length === 0,
		"capabilities",
		() => `${engine} ${record.version} cannot run ${role}: missing ${absent.join(", ")}`,
	);
}

const server = serve();

/** The fixture publisher, whose stalls {@link explainStalls} reports beside the player's. */
let fixturePage: Page | undefined;
const browsers: Browser[] = [];

// One browser per role. Chromium's default autoplay behavior varies with engagement and when the
// graph is built, so require a gesture in this QA run. Pages in one browser share a renderer
// scheduler, and the one that is not frontmost is throttled and reported hidden. Chromium plays to
// its fake output clock: on the private null sink interop.sh provides for Firefox, Chromium's graph
// barely advances.
async function browserFor(role: Role, args: string[] = []): Promise<Browser> {
	const engine = role === "publisher" ? publisherEngine : playerEngine;
	console.error(`=== launching ${engine} ${role} ===`);
	const browser = await launch(
		engine === "chromium"
			? ["--autoplay-policy=document-user-activation-required", "--disable-audio-output", ...args]
			: [],
		engine,
	);
	browsers.push(browser);
	await capabilities(browser, engine, role);
	return browser;
}

/** How a subscriber page is set up. */
type SubscriberProps = {
	muted?: boolean;
	/** Start a Playwright trace. Defaults to true. */
	trace?: boolean;
	/** The player's `delay` attribute, when not its adaptive default. */
	delay?: string;
	/** Pin the reader independently of the epoch-aware publishers. */
	versions?: string;
};

/** Open a subscriber page and wait for the player to start sampling. Never reloads. */
async function subscriber(
	broadcast: string,
	label: string,
	{ muted = false, trace = true, delay, versions }: SubscriberProps = {},
): Promise<[Page, BrowserErrors]> {
	// visible="always" because the window is never frontmost in a headless run, and the default
	// policy would stop downloading video and leave the canvas black.
	const params: Record<string, string> = {
		url: relayFor(playerEngine),
		broadcast,
		visible: "always",
		muted: String(muted),
	};
	if (delay !== undefined) params.delay = delay;
	if (versions !== undefined) params.versions = versions;
	const [page, errors] = await open(
		await browserFor("player"),
		pageUrl(server.origin, "subscribe", params),
		label,
		trace,
	);
	await waitForWatch(page);
	return [page, errors];
}

// A real click, so the page carries user activation. Every audio path is gated on it.
const gesture = (page: Page) => page.mouse.click(1, 1);

/** A denied browser permission is visible, silent to discovery, and revives on a later grant. */
async function captureDenial(broadcast: string): Promise<void> {
	console.error("=== capture denial and grant ===");
	const [viewer, viewerErrors] = await subscriber(broadcast, "capture viewer", { muted: true });
	const [publisher, publisherErrors] = await open(
		await browserFor("publisher", ["--use-fake-device-for-media-stream"]),
		pageUrl(server.origin, "publish", { url: relayFor(publisherEngine), broadcast }),
		"capture publisher",
		false,
		{ permissions: [] },
	);

	const denied = await waitFor(publisher, publisherErrors, readCaptureState, {
		deadline: Date.now() + timeoutMs,
		assertion: "capture denial is visible",
		description: "both refused capture sources to report NotAllowedError",
		predicate: (state) => state.videoError === "NotAllowedError" && state.audioError === "NotAllowedError",
	});
	check(
		!denied.videoActive && !denied.audioActive,
		"capture denial holds publication",
		() => `denied capture exposed media: ${JSON.stringify(denied)}`,
	);

	const absent = await collect(viewer, viewerErrors, 1000);
	check(
		absent.length > 0 &&
			absent.every(
				(state) =>
					!state.broadcastActive &&
					state.broadcastStatus === "offline" &&
					!state.hasAudio &&
					state.videoFrames === 0,
			),
		"capture denial holds publication",
		() => `a viewer saw a partial or announced broadcast: ${JSON.stringify(absent.at(-1))}`,
	);

	await publisher.context().grantPermissions(["camera"]);
	const partial = await waitFor(publisher, publisherErrors, readCaptureState, {
		deadline: Date.now() + timeoutMs,
		assertion: "partial capture is withheld",
		description: "camera capture while microphone permission remains denied",
		predicate: (state) => state.videoActive && state.audioError === "NotAllowedError",
	});
	check(!partial.audioActive, "partial capture is withheld", () => JSON.stringify(partial));
	const withheld = await collect(viewer, viewerErrors, 1000);
	check(
		withheld.every(
			(state) => !state.broadcastActive && state.broadcastStatus === "offline" && state.videoFrames === 0,
		),
		"partial capture is withheld",
		() => `viewer saw a video-only broadcast despite microphone denial: ${JSON.stringify(withheld.at(-1))}`,
	);

	await publisher.context().grantPermissions(["camera", "microphone"]);
	await waitFor(publisher, publisherErrors, readCaptureState, {
		deadline: Date.now() + timeoutMs,
		assertion: "capture grant recovers",
		description: "camera and microphone to capture after permission changes without a reload",
		predicate: (state) => state.videoActive && state.audioActive && !state.videoError && !state.audioError,
	});
	await waitForState(viewer, viewerErrors, {
		deadline: Date.now() + timeoutMs,
		assertion: "capture grant announces",
		description: "the previously absent broadcast to announce and encode video",
		predicate: (state) => state.broadcastActive && state.videoFrames > 0,
	});
	const navigations = await publisher.evaluate(() => performance.getEntriesByType("navigation").length);
	check(navigations === 1, "capture grant recovers", () => `publisher reloaded ${navigations} times`);
	console.error("  refused capture stayed absent, then both tracks captured and encoded after grant without reload");
}

let failure: Error | undefined;
// Set when the selected cases cannot run on this host, which is neither a pass nor a failure.
let skipped = false;
try {
	const broadcast = values.broadcast ?? `media-${process.pid}.hang`;

	// ── publisher ────────────────────────────────────────────────────────────
	// Only the fixture's cases read these, and none of them can be selected with a native publisher.
	let publisher!: Page;
	let publisherErrors!: BrowserErrors;
	if (native) {
		console.error(`=== native publisher: ${broadcast} ===`);
	} else {
		const browser = await browserFor("publisher");
		const aac = AAC_CASES.filter(wants);
		if (aac.length > 0 && !observed.get(publisherEngine)?.found["encode mp4a.40.2"]) {
			console.error(
				`SKIP ${aac.join(", ")}: the ${publisherEngine} ${browser.version()} publisher cannot encode AAC (no platform encoder)`,
			);
			skipped = true;
			throw new Error("skipped");
		}
		[publisher, publisherErrors] = await open(
			browser,
			pageUrl(server.origin, "fixture", { url: relayFor(publisherEngine), broadcast, fault }),
			"fixture",
		);
		fixturePage = publisher;

		console.error("=== publisher readiness ===");
		const beforePublisherGesture = await readFixtureState(publisher);
		check(
			beforePublisherGesture.audioState === "suspended" && beforePublisherGesture.frameId < 0,
			"publisher gesture gate",
			() => `fixture started without a gesture: ${JSON.stringify(beforePublisherGesture)}`,
		);
		console.error("  fixture graph suspended before a click under document activation policy");

		await gesture(publisher);
		const ready = await waitForFixture(publisher, publisherErrors, {
			deadline: Date.now() + timeoutMs,
			assertion: "publisher readiness",
			description: "the fixture to announce a video and audio catalog and start its clock",
			predicate: (state) => state.ready && state.frameId > 0,
		});
		console.error(`  announced ${broadcast}, painting from frame ${ready.frameId}`);

		// The fixture must capture the way the probe says production would on this engine. Only the
		// <video> fallback announces itself, so a fallback taken by mistake, or a missed one, fails here.
		const publisherCapture = capturePath(observed.get(publisherEngine)?.found ?? {});
		const fellBack = publisherErrors.warnings.some((warning) => warning.includes(CAPTURE_FALLBACK_WARNING));
		check(
			fellBack === (publisherCapture === "element"),
			"capture path",
			() =>
				`probed ${publisherCapture} capture, but the fixture ${fellBack ? "fell back" : "did not fall back"} to <video>`,
		);
		console.error(`  captured through the ${publisherCapture} path`);
	}

	// ── cold start ───────────────────────────────────────────────────────────
	// No reload anywhere below. The publisher is known ready, so a subscriber that needs a second
	// page load to find the broadcast is an initialization bug, not a race.
	console.error("=== cold start ===");
	// No trace yet. DOM snapshots evaluate with a user gesture, and the player's graph is built
	// after the first of those, so Chromium would start it running and the gate below would fail.
	let [player, playerErrors] = await subscriber(broadcast, "player", { trace: false });

	// Video has to reach the canvas with no gesture at all: only audio is ever gated.
	const first = await waitForState(player, playerErrors, {
		deadline: Date.now() + timeoutMs,
		assertion: "cold start presents video",
		description: "the first presented fixture frame",
		predicate: (state) => state.frameId !== undefined,
	});
	const beforePlayerGesture = await waitForState(player, playerErrors, {
		deadline: Date.now() + timeoutMs,
		assertion: "player gesture gate",
		description: "the player audio graph to exist before a gesture",
		predicate: (state) => state.audioContext !== undefined,
	});
	// WebKit reports a context it has not started as "interrupted", outside the standard states.
	check(
		!beforePlayerGesture.userActivated &&
			(beforePlayerGesture.audioContext === "suspended" || beforePlayerGesture.audioContext === "interrupted") &&
			beforePlayerGesture.toneStep === undefined,
		"player gesture gate",
		() => `player audio ran without a gesture: ${JSON.stringify(beforePlayerGesture)}`,
	);
	console.error(`  presented frame ${first.frameId} before any gesture; player audio stayed suspended`);

	// The context already exists and is suspended. A snapshot sets sticky activation but does not
	// resume it; pointerdown still has to.
	await startTrace(player, "player");
	await gesture(player);
	// Deliberately does not wait for a tone: whether audio actually carries the fixture is what
	// assertMedia measures, so silence has to fail there rather than time out here.
	await waitForState(player, playerErrors, {
		deadline: Date.now() + timeoutMs,
		assertion: "gesture starts audio",
		description: "the audio graph to resume after a user gesture",
		predicate: (state) => state.audioContext === "running",
	});

	if (values.focused) {
		console.error("  focused case run: cold-start grading and unrelated cases are not selected");
		await waitForState(player, playerErrors, {
			deadline: Date.now() + SETTLE_MS,
			assertion: "playback starts",
			description: "the audio buffer to fill before the selected lifecycle case",
			predicate: (state) => state.audioContext === "running" && !state.audioStalled,
		});
	} else {
		await measurePlayback(player, playerErrors, "cold start");
	}

	// A codec changes decoder identity; a VP8 resize does not. Measure through each
	// transition, then keep checking pixels after its first new-generation picture.
	if (wants("rename")) {
		console.error("=== codec identity and coded-size changes ===");
		const before = await readPlayerState(player);
		await command(publisher, "changeVideoCodec");
		const change = await observe(player, playerErrors, "codec rename");
		const changed = change.samples;
		const first = changed.findIndex(
			(sample) =>
				sample.generation === 1 &&
				sample.videoCodec?.startsWith("vp8") &&
				sample.videoTrack !== before.videoTrack,
		);
		await change.grade(() => {
			check(first >= 0, "codec rename", () => `no renamed VP8 picture: ${JSON.stringify(changed.at(-1))}`);
			check(
				changed.slice(first).every((sample) => sample.frameId === undefined || sample.generation === 1),
				"old track picture",
				() => "a pre-switch picture appeared after the new codec's first picture",
			);
		});
		const renamed = changed[first].videoTrack;
		await measurePlayback(player, playerErrors, "after codec rename");

		await command(publisher, "resizeVideo");
		const resize = await observe(player, playerErrors, "coded-size growth");
		const resized = resize.samples;
		const grown = resized.findIndex(
			(sample) =>
				sample.generation === 2 &&
				sample.frameWidth === Pattern.WIDTH * 2 &&
				sample.frameHeight === Pattern.HEIGHT * 2 &&
				sample.videoCodedWidth === Pattern.WIDTH * 2 &&
				sample.videoCodedHeight === Pattern.HEIGHT * 2,
		);
		await resize.grade(() => {
			check(
				grown >= 0,
				"coded-size growth",
				() => `no larger presented picture: ${JSON.stringify(resized.at(-1))}`,
			);
			check(
				resized.every((sample) => sample.videoTrack === renamed && sample.videoCodec?.startsWith("vp8")),
				"resize keeps identity",
				() => `VP8 resize changed track: ${JSON.stringify(resized.at(-1))}`,
			);
			check(
				resized.slice(grown).every((sample) => sample.frameId === undefined || sample.generation === 2),
				"old track picture",
				() => "a pre-resize picture appeared after the first larger picture",
			);
		});
		await measurePlayback(player, playerErrors, "after coded-size growth");
	}

	// ── pause and resume ─────────────────────────────────────────────────────
	if (wants("pause")) {
		console.error("=== pause and resume ===");
		await pause(player);
		await waitForState(player, playerErrors, {
			deadline: Date.now() + SETTLE_MS,
			assertion: "pause takes effect",
			description: "the player to report itself paused",
			predicate: (state) => state.paused && state.controlLabel === "Play",
		});

		// The frame the pause settled on, not the one showing when the flag flipped: a frame already
		// scheduled for presentation still lands, and that is one frame, not playback continuing.
		const paused = await waitFrozen(
			player,
			playerErrors,
			"pause holds the picture",
			"the presented frame never settled after pausing",
		);

		const held = await collect(player, playerErrors, HELD_MS);
		const moved = held.filter((s) => s.frameId !== undefined && s.frameId !== paused);
		check(
			moved.length === 0,
			"pause holds the picture",
			() => `presented frame moved from ${paused} to ${moved[moved.length - 1]?.frameId} while paused`,
		);
		check(
			held[held.length - 1]?.painted === true,
			"pause holds the picture",
			() => `pause cleared the preview frame: ${JSON.stringify(held[held.length - 1])}`,
		);

		await player.locator(SELECTORS.ui).locator(SELECTORS.centerPlay).click();
		const resumed = await waitForState(player, playerErrors, {
			deadline: Date.now() + SETTLE_MS,
			assertion: "resume takes effect",
			description: `the presented frame to move past the ${paused} the pause held`,
			predicate: (state) => !state.paused && (state.frameId ?? 0) > paused,
		});
		console.error(`  resumed ${(resumed.frameId ?? 0) - paused} frames past the pause`);
		await measurePlayback(player, playerErrors, "after resume");
	}

	// ── disable and re-enable the video rendition ────────────────────────────
	// A disabled rendition stays in the catalog with `enabled: false`. The player deselects it and
	// shows black instead of freezing on the last picture, then picks it up again once re-enabled.
	if (wants("disable")) {
		console.error("=== disable and enable video ===");
		const before = await readPlayerState(player);
		await command(publisher, "disableVideo");
		await waitForFixture(publisher, publisherErrors, {
			deadline: Date.now() + SETTLE_MS,
			assertion: "disable keeps the rendition",
			description: "the fixture to publish its video rendition with enabled: false and stop encoding",
			predicate: (state) => !state.videoEnabled && !state.videoActive,
		});
		await waitForState(player, playerErrors, {
			deadline: Date.now() + SETTLE_MS,
			assertion: "disable blanks the picture",
			description: `the canvas to go black instead of holding frame ${before.frameId}`,
			predicate: (state) => !state.painted,
		});
		const blank = await collect(player, playerErrors, HELD_MS);
		const lit = blank.filter((s) => s.painted);
		check(
			lit.length === 0,
			"disable blanks the picture",
			() => `the canvas showed a picture again while disabled: ${JSON.stringify(lit[0])}`,
		);

		await command(publisher, "enableVideo");
		const resumed = await waitForState(player, playerErrors, {
			deadline: Date.now() + timeoutMs,
			assertion: "enable resumes playback",
			description: `the presented frame to move past the ${before.frameId} showing before the disable`,
			predicate: (state) => (state.frameId ?? 0) > (before.frameId ?? 0),
		});
		console.error(`  black while disabled, then resumed at frame ${resumed.frameId}`);
		await measurePlayback(player, playerErrors, "after enable");
	}

	// ── unsubscribe and rejoin ───────────────────────────────────────────────
	if (wants("rejoin")) {
		console.error("=== unsubscribe and rejoin ===");
		await player.locator(SELECTORS.watch).evaluate((el) => el.setAttribute("name", "media-nowhere.hang"));
		const left = await waitFrozen(
			player,
			playerErrors,
			"unsubscribe stops playback",
			"the player kept presenting a broadcast it no longer subscribes to",
		);

		await player.locator(SELECTORS.watch).evaluate((el, name) => el.setAttribute("name", name), broadcast);
		await waitForState(player, playerErrors, {
			deadline: Date.now() + timeoutMs,
			assertion: "rejoin resumes playback",
			description: `the presented frame to move past the ${left} it stopped on`,
			predicate: (state) => (state.frameId ?? 0) > left,
		});
		await waitForLive(publisher, publisherErrors, player, playerErrors, "rejoin reaches live");
		await measurePlayback(player, playerErrors, "after rejoin");
	}

	// ── detach and reattach ──────────────────────────────────────────────────
	if (wants("detach")) {
		console.error("=== detach and reattach ===");
		const busy = await readPlayerState(player);
		// Its own assertion name, not "resource baseline": the leaked-session control expects that one
		// to fail, so sharing it would let broken instrumentation satisfy the control without the
		// leaking detach ever running.
		check(
			busy.resources.transports + busy.resources.sockets > 0 && busy.resources.audioContexts > 0,
			"resource instrumentation",
			() => `the player holds nothing to release while playing: ${JSON.stringify(busy.resources)}`,
		);

		if (values.leak) {
			await command(player, "startLeak");
			await waitForResources(player, playerErrors, {
				deadline: Date.now() + SETTLE_MS,
				assertion: "resource instrumentation",
				description: `the deliberately leaked player to open another audio graph beyond ${JSON.stringify(busy.resources)}`,
				predicate: (r) => leakedPlayerStarted(busy.resources, r),
			});
		}
		await command(player, "detach");
		await waitForResources(player, playerErrors, {
			deadline: Date.now() + SETTLE_MS,
			assertion: "resource baseline",
			description: `every session, audio graph, and worker to be released (held ${JSON.stringify(busy.resources)} while playing)`,
			predicate: (r) => r.transports === 0 && r.sockets === 0 && r.audioContexts === 0 && r.workers === 0,
		});
		console.error("  detach released every session, audio graph, and worker");

		await command(player, "reattach");
		await waitForLive(publisher, publisherErrors, player, playerErrors, "reattach reaches live", true);
		await measurePlayback(player, playerErrors, "after reattach");
	}

	// These publishers overlap: neither a successful new picture nor fallback
	// can be explained by the incumbent timing out or closing its session.
	if (wants("replacement")) {
		const sourceBrowser = await browserFor("publisher");
		for (const version of ["moq-lite-07-wip", "moq-lite-06", "moq-transport-22"]) {
			console.error(`=== live replacement and fallback: ${version} ===`);
			const name = `replacement-${version}-${process.pid}.hang`;
			const [old, oldErrors] = await open(
				sourceBrowser,
				pageUrl(server.origin, "fixture", {
					url: relayFor(publisherEngine),
					broadcast: name,
					versions: "moq-lite-07-wip",
					generation: "0",
				}),
				`old-${version}`,
			);
			let next: Page | undefined;
			let viewer: Page | undefined;
			try {
				await gesture(old);
				await waitForFixture(old, oldErrors, {
					deadline: Date.now() + timeoutMs,
					assertion: "incumbent ready",
					description: "the incumbent catalog and clock",
					predicate: (s) => s.ready && s.frameId > 0,
				});
				const [watch, watchErrors] = await subscriber(name, `replacement-${version}`, {
					versions: version,
					delay: `${REPLACEMENT_DELAY_MS}ms`,
				});
				viewer = watch;
				await gesture(watch);
				const first = await waitForState(watch, watchErrors, {
					deadline: Date.now() + timeoutMs,
					assertion: "incumbent plays",
					description: "the incumbent's marked pixels",
					predicate: (s) => s.generation === 0 && !s.audioStalled,
				});
				check(
					version === "moq-lite-06" ? first.broadcastEpoch === undefined : first.broadcastEpoch !== undefined,
					"negotiated epoch identity",
					() => JSON.stringify(first),
				);
				const before = await readFixtureState(old);
				const [replacement, replacementErrors] = await open(
					sourceBrowser,
					pageUrl(server.origin, "fixture", {
						url: relayFor(publisherEngine),
						broadcast: name,
						versions: "moq-lite-07-wip",
						generation: "1",
					}),
					`new-${version}`,
				);
				next = replacement;
				await gesture(replacement);
				const { readyAt: ready } = await waitForFixture(replacement, replacementErrors, {
					deadline: Date.now() + timeoutMs,
					assertion: "replacement ready",
					description: "the replacement catalog and clock",
					predicate: (s) => s.ready && s.frameId > 0,
				});
				// The page shares the host clock, so timing from its own readiness skips the driver's poll lag.
				if (ready === undefined) throw new Error("a ready fixture reports when it became ready");
				const switched = await waitForState(watch, watchErrors, {
					deadline: ready + SWITCH_MS,
					assertion: "live replacement plays",
					description: `the new run's picture and synchronized tone within ${SWITCH_MS}ms while its incumbent stays open`,
					predicate: (s) => s.generation === 1 && !s.audioStalled && playsInSync(s),
				});
				const replacementMs = Date.now() - ready;
				if (version !== "moq-lite-06")
					check(switched.broadcastEpoch !== first.broadcastEpoch, "replacement has a fresh epoch", () =>
						JSON.stringify(switched),
					);
				const during = await collect(watch, watchErrors, WINDOW_MS);
				check(
					during.every((s) => s.frameId === undefined || s.generation === 1),
					"old run never splices",
					() => JSON.stringify(during.filter((s) => s.frameId !== undefined && s.generation !== 1)),
				);
				await measurePlayback(watch, watchErrors, `replacement ${version}`);
				const held = await readFixtureState(old);
				check(held.ready && held.frameId > before.frameId, "incumbent stays live", () => JSON.stringify(held));
				const withdrawn = Date.now();
				await command(replacement, "stop");
				const back = await waitForState(watch, watchErrors, {
					deadline: withdrawn + SWITCH_MS,
					assertion: "live incumbent fallback plays",
					description: `the still-live older run's picture and synchronized tone within ${SWITCH_MS}ms of the newest withdrawing`,
					predicate: (s) => s.generation === 0 && !s.audioStalled && playsInSync(s),
				});
				const fallbackMs = Date.now() - withdrawn;
				check(back.broadcastEpoch === first.broadcastEpoch, "fallback keeps incumbent identity", () =>
					JSON.stringify(back),
				);
				await measurePlayback(watch, watchErrors, `fallback ${version}`);
				console.error(
					`  ${version}: replacement ${replacementMs}ms, fallback ${fallbackMs}ms, bound ${SWITCH_MS}ms`,
				);
			} finally {
				await viewer?.context().close();
				await next?.context().close();
				await old.context().close();
			}
		}
	}

	// ── publisher stop and same-path republish ───────────────────────────────
	if (wants("republish")) {
		console.error("=== stop and republish ===");
		const before = await readPlayerState(player);
		await command(publisher, "stop");
		await waitFrozen(
			player,
			playerErrors,
			"playback stops with the publisher",
			"the player kept presenting new frames after the publisher went away",
		);

		await command(publisher, "start");
		// The restarted fixture paints from zero again, so a recovered player is one presenting a
		// frame from before the stop: proof it followed the new announcement rather than a cache.
		const recovered = await waitForState(player, playerErrors, {
			deadline: Date.now() + timeoutMs,
			assertion: "republish serves the new stream",
			description: `a presented frame below the ${before.frameId} reached before the publisher stopped`,
			predicate: (state) => state.frameId !== undefined && state.frameId < (before.frameId ?? 0),
		});
		console.error(`  recovered at frame ${recovered.frameId}, restarted from ${before.frameId}`);
		await measurePlayback(player, playerErrors, "after republish");
	}

	// ── late join ────────────────────────────────────────────────────────────
	// A fresh page against a broadcast that has been running for a while, after a demand gap. The
	// relay may hand over the GOP from before the gap, so the first frame can be stale; the player
	// has to show it promptly and then fast-forward to live rather than replay what it missed.
	if (wants("late-join")) {
		console.error("=== late join ===");
		await player.close();
		[player, playerErrors] = await subscriber(broadcast, "latecomer", {
			delay: values.lag ? LAGGED_DELAY : undefined,
		});
		const joined = Date.now();
		await gesture(player);

		const shown = await waitForState(player, playerErrors, {
			deadline: joined + LATE_START_MS,
			assertion: "late join shows video",
			description: `a presented fixture frame within ${LATE_START_MS}ms of the latecomer loading`,
			predicate: (state) => state.frameId !== undefined,
		});
		const startMs = Date.now() - joined;

		const live = await waitForLive(publisher, publisherErrors, player, playerErrors, "late join reaches live");
		console.error(
			`  showed frame ${shown.frameId} after ${startMs}ms, ${framesToMs(live.first)}ms behind the fixture; ` +
				`live ${live.after}ms later, ${framesToMs(live.lag)}ms behind`,
		);
		await measurePlayback(player, playerErrors, "late join");
	}

	// ── audio format changes ─────────────────────────────────────────────────
	// A rate or channel-count change re-mints the audio rendition's track; the player has to follow
	// it without its audio stalling or slipping out of sync with the picture.
	if (wants("audio-format")) {
		await followAudio(
			player,
			playerErrors,
			"audio sample-rate change",
			() => command(publisher, "changeAudioRate"),
			(s, before) => s.audioSampleRate === CHANGED_SAMPLE_RATE && s.audioTrack !== before.audioTrack,
		);
		await measurePlayback(player, playerErrors, "after sample-rate change");
		await followAudio(
			player,
			playerErrors,
			"audio channel-count change",
			() => command(publisher, "changeAudioChannels"),
			(s, before) => s.audioChannels === 2 && s.audioTrack !== before.audioTrack,
		);
		await measurePlayback(player, playerErrors, "after channel-count change");
	}

	if (wants("audio-codec")) {
		await followAudio(
			player,
			playerErrors,
			"audio codec change",
			() => command(publisher, "changeAudioCodec"),
			(s, before) => s.audioCodec?.startsWith("mp4a") === true && s.audioTrack !== before.audioTrack,
		);
		await measurePlayback(player, playerErrors, "after audio codec change");
	}

	// ── audio rendition switch ───────────────────────────────────────────────
	// The fixture adds a second rendition of the same tone; a native publisher already sends one per
	// codec, so there the switch also changes codec.
	if (wants("audio-switch")) {
		if (!native) await command(publisher, "addAudioRendition");
		const renditions = await waitFor(player, playerErrors, audioRenditions, {
			deadline: Date.now() + SETTLE_MS,
			assertion: "audio rendition switch",
			description: "a second audio rendition in the player's catalog",
			predicate: (names) => names.length >= 2,
		});
		const from = (await readPlayerState(player)).audioRendition;
		const to = native ? renditions.find((name) => name !== from) : ALT_AUDIO;
		if (to === undefined || from === undefined) throw new Error(`no rendition to switch to from ${from}`);
		for (const [name, back] of [
			[to, false],
			[from, true],
		] as const) {
			await followAudio(
				player,
				playerErrors,
				back ? "audio rendition switch back" : "audio rendition switch",
				() => targetAudio(player, name),
				(s) => s.audioRendition === name,
			);
		}
		await targetAudio(player, undefined);
		if (!native) await command(publisher, "removeAudioRendition");
		await measurePlayback(player, playerErrors, "after audio rendition switch");
	}

	// ── mute and volume ──────────────────────────────────────────────────────
	// Graded where samples reach the speakers, after the player's own gain: the graph root the other
	// cases read sits upstream of it.
	if (wants("mute-volume")) {
		console.error("=== mute and volume ===");
		await checkVolume(player, playerErrors, 1);
		await checkVolume(player, playerErrors, 0.25);

		await player.locator(SELECTORS.watch).evaluate((el: MoqWatch) => {
			el.muted = true;
		});
		await sleep(VOLUME_SETTLE_MS);
		const muted = await collect(player, playerErrors, HELD_MS);
		const loud = muted.filter((s) => (s.speakerRms ?? 0) >= AUDIBLE_RMS);
		check(
			muted.length > 0 && loud.length === 0,
			"mute silences the output",
			() =>
				`${loud.length}/${muted.length} muted samples reached the speakers, up to ${Math.max(...loud.map((s) => s.speakerRms ?? 0)).toFixed(3)} rms`,
		);
		const shown = muted.filter((s) => s.frameId !== undefined);
		check(
			shown.length >= 2 && (shown.at(-1)?.frameId ?? 0) > (shown[0].frameId ?? 0),
			"mute keeps the picture playing",
			() => `the picture stopped while muted: ${JSON.stringify(muted.at(-1))}`,
		);

		await player.locator(SELECTORS.watch).evaluate((el: MoqWatch) => {
			el.muted = false;
			el.volume = 0.5;
		});
		await waitForState(player, playerErrors, {
			deadline: Date.now() + SETTLE_MS,
			assertion: "unmute restores the output",
			description: "the tone to reach the speakers again after unmuting",
			predicate: (s) => (s.speakerRms ?? 0) >= AUDIBLE_RMS,
		});
		console.error(`  muted: speakers silent for ${muted.length} samples while the picture kept playing`);
		await measurePlayback(player, playerErrors, "after unmute");
	}

	// ── browser AAC publish ──────────────────────────────────────────────────
	// The fixture publishing AAC from its first frame, on a broadcast of its own.
	if (wants("aac")) {
		console.error("=== AAC publish ===");
		const name = `${broadcast}-aac.hang`;
		const [source, sourceErrors] = await open(
			await browserFor("publisher"),
			pageUrl(server.origin, "fixture", {
				url: relayFor(publisherEngine),
				broadcast: name,
				"audio-codec": "aac",
			}),
			"aac-fixture",
		);
		let viewer: Page | undefined;
		try {
			await gesture(source);
			await waitForFixture(source, sourceErrors, {
				deadline: Date.now() + timeoutMs,
				assertion: "AAC publish",
				description: "the AAC fixture to announce an AAC audio catalog and start its clock",
				predicate: (s) => s.ready && s.frameId > 0 && s.audio.audio?.codec.startsWith("mp4a") === true,
			});
			const [watch, watchErrors] = await subscriber(name, "aac-player");
			viewer = watch;
			await gesture(watch);
			await waitForState(watch, watchErrors, {
				deadline: Date.now() + timeoutMs,
				assertion: "AAC publish",
				description: "the player to select the AAC rendition",
				predicate: (s) => s.audioCodec?.startsWith("mp4a") === true,
			});
			await measurePlayback(watch, watchErrors, "AAC publish");
		} finally {
			await viewer?.context().close();
			await source.context().close();
		}
	}

	if (wants("capture-denial")) await captureDenial(`${broadcast}-capture.hang`);
} catch (err) {
	if (!skipped) failure = err instanceof Error ? err : new Error(String(err));
}

// The verdict decides whether the trace is evidence: a negative control passes by failing, so a
// caught error alone is not a failure.
let code = 0;
if (skipped) {
	code = SKIPPED;
} else if (expectFail !== undefined) {
	// A negative control. The run has to fail, and fail on the assertion it was aimed at: a pass, or
	// a failure somewhere else, both mean the assertion does not measure what it claims to.
	if (!failure) {
		console.error(`negative control passed, but it must fail on "${expectFail}"`);
		code = 1;
	} else if (!(failure instanceof Failure) || failure.assertion !== expectFail) {
		console.error(`negative control was aimed at "${expectFail}" but broke elsewhere: ${failure.message}`);
		code = 1;
	} else {
		console.error(`negative control failed as required: ${failure.message}`);
	}
} else if (failure) {
	console.error(`FAIL ${failure.message}`);
	code = 1;
} else {
	console.error(
		`media: ${playerEngine} player over ${playerEngine === "chromium" ? "automatic transport selection" : "WebSocket fallback"} measured; all checks passed (${native ? "native publisher" : `${publisherEngine} publisher, ${capturePath(observed.get(publisherEngine)?.found ?? {})} capture`})`,
	);
}

await finishTraces(code !== 0);
for (const browser of browsers) await browser.close().catch(() => {});
server.stop();
process.exit(code);
