// Measures silence-suppression routes for the browser Opus publisher. Posts results to /result.
const RATE = 48000;
const FRAME = 960; // 20 ms
const FRAME_US = 20_000;
const BASE_US = 5_000_000; // a nonzero capture-clock origin, like a real AudioContext time
const GATE_DB = -45; // open above this frame RMS (room tone sits at -60)
const GATE_HANGOVER = 10; // frames kept open after the last loud one, as libopus DTX waits 200 ms
const params = new URLSearchParams(location.search);
const BROWSER = params.get("browser") ?? "unknown";
const RUNS = Number(params.get("runs") ?? 5);
const log = (...args) => {
	console.log(...args);
	document.body.textContent += `${args.join(" ")}\n`;
	fetch(`/log?browser=${encodeURIComponent(BROWSER)}`, { method: "POST", body: args.join(" ") }).catch(() => {});
};
log("start");

const signal = new Float32Array(await (await fetch("signal.f32")).arrayBuffer());
const frames = [];
for (let i = 0; i * FRAME < signal.length; i++) frames.push(signal.subarray(i * FRAME, (i + 1) * FRAME));

function rmsDb(pcm) {
	let sum = 0;
	for (const v of pcm) sum += v * v;
	return 10 * Math.log10(sum / pcm.length + 1e-20);
}

// The gate: a frame is sent while its RMS is above GATE_DB or within GATE_HANGOVER frames after one.
function gate() {
	const t0 = performance.now();
	const open = [];
	let hang = 0;
	for (const pcm of frames) {
		const loud = rmsDb(pcm) > GATE_DB;
		// Pre-roll: opening also sends the frame before, which holds a soft onset's first milliseconds.
		if (loud && hang === 0 && open.length > 0) open[open.length - 1] = true;
		if (loud) hang = GATE_HANGOVER + 1;
		open.push(hang > 0);
		hang = Math.max(0, hang - 1);
	}
	return { open, ms: performance.now() - t0 };
}
const gated = gate();

const opusConfig = (dtx) => ({
	codec: "opus",
	sampleRate: RATE,
	numberOfChannels: 1,
	bitrate: 32_000,
	opus: { frameDuration: FRAME_US, application: "voip", signal: "voice", ...(dtx ? { usedtx: true } : {}) },
});

const b64 = (bytes) => {
	let s = "";
	for (const b of bytes) s += String.fromCharCode(b);
	return btoa(s);
};

function audioData(i) {
	return new AudioData({
		format: "f32-planar",
		sampleRate: RATE,
		numberOfFrames: FRAME,
		numberOfChannels: 1,
		timestamp: BASE_US + i * FRAME_US,
		data: frames[i].slice(),
	});
}

// Pace like a capture graph: submit each frame once its wall-clock time arrives.
async function pace(fn) {
	const start = performance.now();
	for (let i = 0; i < frames.length; i++) {
		const due = start + (i + 1) * 20;
		const wait = due - performance.now();
		if (wait > 0) await new Promise((r) => setTimeout(r, wait));
		fn(i);
	}
}

// One WebCodecs run. `feed(i)` says whether frame i reaches the encoder; `stamp` picks the timestamp
// source: "queue" maps chunk n to the n-th input fed, "chunk" trusts chunk.timestamp.
async function webcodecs({ dtx, feed, stamp, paced }) {
	const fed = []; // { i, at }
	const packets = [];
	let error;
	const encoder = new AudioEncoder({
		output: (chunk) => {
			const bytes = new Uint8Array(chunk.byteLength);
			chunk.copyTo(bytes);
			const n = packets.length;
			const input = fed[n];
			packets.push({
				ts: stamp === "queue" ? (input ? BASE_US + input.i * FRAME_US : null) : chunk.timestamp,
				raw: chunk.timestamp,
				dur: chunk.duration ?? null,
				lagMs: input ? performance.now() - input.at : null,
				input: input?.i ?? null,
				bytes: paced ? b64(bytes) : undefined,
				len: bytes.length,
			});
		},
		error: (e) => {
			error = String(e);
		},
	});
	encoder.configure(opusConfig(dtx));
	const t0 = performance.now();
	const submit = (i) => {
		if (!feed(i)) return;
		fed.push({ i, at: performance.now() });
		const data = audioData(i);
		encoder.encode(data);
		data.close();
	};
	if (paced) await pace(submit);
	else for (let i = 0; i < frames.length; i++) submit(i);
	const beforeFlush = packets.length;
	await encoder.flush();
	const ms = performance.now() - t0;
	encoder.close();
	return { packets, fed: fed.length, beforeFlush, ms, error };
}

// The WASM route: libopus 1.6.1 with DTX on, stamped from the frame index.
const wasmBytes = await (await fetch("enc.wasm")).arrayBuffer();
const wasmModule = await WebAssembly.compile(wasmBytes);
async function wasm({ paced }) {
	const { exports: x } = await WebAssembly.instantiate(wasmModule, {});
	x._initialize?.();
	const OPUS_APPLICATION_VOIP = 2048;
	const OPUS_SIGNAL_VOICE = 3001;
	const enc = x.enc_create(RATE, 1, OPUS_APPLICATION_VOIP, 32_000, 1, -1, OPUS_SIGNAL_VOICE);
	if (!enc) throw new Error("enc_create failed");
	const pcmPtr = x.mem_alloc(FRAME * 4);
	const outPtr = x.mem_alloc(1500);
	const packets = [];
	const encodeMs = [];
	let dtxFrames = 0;
	const submit = (i) => {
		const t = performance.now();
		new Float32Array(x.memory.buffer, pcmPtr, FRAME).set(frames[i]);
		const len = x.enc_encode(enc, pcmPtr, FRAME, outPtr, 1500);
		encodeMs.push(performance.now() - t);
		if (len < 0) throw new Error(`opus_encode failed: ${len}`);
		// A packet of two bytes or fewer carries no audio: DTX says don't transmit it.
		if (len <= 2) {
			dtxFrames++;
			return;
		}
		const bytes = new Uint8Array(x.memory.buffer, outPtr, len).slice();
		packets.push({ ts: BASE_US + i * FRAME_US, raw: null, dur: FRAME_US, lagMs: 0, input: i, bytes: paced ? b64(bytes) : undefined, len });
	};
	const t0 = performance.now();
	if (paced) await pace(submit);
	else for (let i = 0; i < frames.length; i++) submit(i);
	const ms = performance.now() - t0;
	const lookahead = x.enc_lookahead(enc);
	x.enc_destroy(enc);
	const sorted = [...encodeMs].sort((a, b) => a - b);
	return {
		packets,
		fed: frames.length,
		beforeFlush: packets.length,
		ms,
		dtxFrames,
		lookahead,
		encodeMsP50: sorted[Math.floor(sorted.length / 2)],
		encodeMsP99: sorted[Math.floor(sorted.length * 0.99)],
		encodeMsMax: sorted[sorted.length - 1],
	};
}

const routes = {
	baseline: (paced) => webcodecs({ dtx: false, feed: () => true, stamp: "chunk", paced }),
	gate: (paced) => webcodecs({ dtx: false, feed: (i) => gated.open[i], stamp: "queue", paced }),
	"dtx-native": (paced) => webcodecs({ dtx: true, feed: () => true, stamp: "chunk", paced }),
	wasm: (paced) => wasm({ paced }),
};

const result = { browser: BROWSER, userAgent: navigator.userAgent, gateMs: gated.ms, gateOpen: gated.open.filter(Boolean).length, frames: frames.length, support: {}, fast: {}, paced: {} };
try {
	for (const dtx of [false, true]) {
		const s = await AudioEncoder.isConfigSupported(opusConfig(dtx));
		result.support[dtx ? "dtx" : "plain"] = { supported: s.supported, config: s.config };
	}
	log("support", JSON.stringify(result.support));
	// Warm up each path once so first-run JIT and codec init stay out of the timings.
	for (const name of Object.keys(routes)) await routes[name](false);
	for (let run = 0; run < RUNS; run++) {
		const order = Object.keys(routes);
		if (run % 2) order.reverse();
		for (const name of order) {
			const r = await routes[name](false);
			(result.fast[name] ??= []).push({ ms: r.ms, packets: r.packets.length, fed: r.fed, beforeFlush: r.beforeFlush, encodeMsP50: r.encodeMsP50, encodeMsP99: r.encodeMsP99, encodeMsMax: r.encodeMsMax, error: r.error });
			log("fast", name, run, r.ms.toFixed(1), r.packets.length);
		}
	}
	for (const name of Object.keys(routes)) {
		log("paced", name);
		result.paced[name] = await routes[name](true);
	}
} catch (e) {
	result.error = `${e}\n${e.stack ?? ""}`;
	log("error", result.error);
}
await fetch(`/result?browser=${encodeURIComponent(BROWSER)}`, { method: "POST", body: JSON.stringify(result) });
log("done");
