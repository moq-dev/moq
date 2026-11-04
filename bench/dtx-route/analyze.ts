// Decode each route's packets with libopus, place them on the timeline by their stamps, and grade them.
// Usage: bun analyze.ts <www> <decoder.wasm> <wavdir> <result.json>...
import { readFileSync, writeFileSync } from "node:fs";

const [www, decoderPath, wavDir, ...files] = process.argv.slice(2);
const RATE = 48000;
const FRAME = 960;
const BASE_US = 5_000_000;
const MS = RATE / 1000;
const THRESH = 10 ** (-45 / 20); // onset threshold, RMS over 1 ms

const input = new Float32Array(readFileSync(`${www}/signal.f32`).buffer.slice(0));
const { segments } = JSON.parse(readFileSync(`${www}/segments.json`, "utf8")) as {
	segments: { kind: string; start: number; end: number }[];
};
const silent = segments.filter((s) => s.kind.startsWith("silence"));
const active = segments.filter((s) => !s.kind.startsWith("silence"));

const mod = new WebAssembly.Module(readFileSync(decoderPath));
// biome-ignore lint/suspicious/noExplicitAny: raw wasm exports
const x = new WebAssembly.Instance(mod, {}).exports as any;
x._initialize?.();

function decode(packets: { ts: number; bytes: string }[]): Float32Array {
	const out = new Float32Array(input.length + RATE);
	const dec = x.dec_create(RATE, 1);
	const inPtr = x.mem_alloc(1500);
	const pcmPtr = x.mem_alloc(FRAME * 4 * 6);
	for (const p of packets) {
		const bytes = Buffer.from(p.bytes, "base64");
		new Uint8Array(x.memory.buffer, inPtr, bytes.length).set(bytes);
		const n = x.dec_decode(dec, inPtr, bytes.length, pcmPtr, FRAME * 6);
		if (n < 0) throw new Error(`decode failed: ${n}`);
		const at = Math.round(((p.ts - BASE_US) / 1e6) * RATE);
		const pcm = new Float32Array(x.memory.buffer, pcmPtr, n);
		for (let i = 0; i < n; i++) {
			const j = at + i;
			if (j >= 0 && j < out.length) out[j] += pcm[i];
		}
	}
	return out;
}

// 1 ms RMS envelope.
function envelope(pcm: Float32Array): Float32Array {
	const env = new Float32Array(Math.floor(pcm.length / MS));
	for (let k = 0; k < env.length; k++) {
		let s = 0;
		for (let i = k * MS; i < (k + 1) * MS; i++) s += pcm[i] * pcm[i];
		env[k] = Math.sqrt(s / MS);
	}
	return env;
}

const envIn = envelope(input);

// In-place radix-2 FFT; inverse when `inv`.
function fft(re: Float64Array, im: Float64Array, inv: boolean) {
	const n = re.length;
	for (let i = 1, j = 0; i < n; i++) {
		let bit = n >> 1;
		for (; j & bit; bit >>= 1) j ^= bit;
		j ^= bit;
		if (i < j) {
			[re[i], re[j]] = [re[j], re[i]];
			[im[i], im[j]] = [im[j], im[i]];
		}
	}
	for (let len = 2; len <= n; len <<= 1) {
		const ang = ((inv ? 2 : -2) * Math.PI) / len;
		const wr = Math.cos(ang);
		const wi = Math.sin(ang);
		for (let i = 0; i < n; i += len) {
			let cr = 1;
			let ci = 0;
			for (let k = 0; k < len / 2; k++) {
				const ar = re[i + k + len / 2] * cr - im[i + k + len / 2] * ci;
				const ai = re[i + k + len / 2] * ci + im[i + k + len / 2] * cr;
				re[i + k + len / 2] = re[i + k] - ar;
				im[i + k + len / 2] = im[i + k] - ai;
				re[i + k] += ar;
				im[i + k] += ai;
				const t = cr * wr - ci * wi;
				ci = cr * wi + ci * wr;
				cr = t;
			}
		}
	}
	if (inv) for (let i = 0; i < n; i++) re[i] /= n;
}

const N = 1 << 21;
// The lag (ms, output minus input) whose waveform best matches a segment's first second, normalized by
// the output's local energy so a louder region doesn't win, searched over [lo, hi] ms.
function align(out: Float32Array, seg: { start: number; end: number }, lo: number, hi: number): number {
	const a = seg.start;
	const len = Math.min(seg.end - seg.start, RATE);
	const xr = new Float64Array(N);
	const xi = new Float64Array(N);
	const yr = new Float64Array(N);
	const yi = new Float64Array(N);
	for (let i = 0; i < len; i++) xr[i] = input[a + i];
	for (let i = 0; i < out.length && i < N; i++) yr[i] = out[i];
	fft(xr, xi, false);
	fft(yr, yi, false);
	// corr[t] = sum_i x[i] y[t + i]  =>  conj(X) * Y
	for (let k = 0; k < N; k++) {
		const r = xr[k] * yr[k] + xi[k] * yi[k];
		const m = xr[k] * yi[k] - xi[k] * yr[k];
		yr[k] = r;
		yi[k] = m;
	}
	fft(yr, yi, true);
	const energy = new Float64Array(out.length + 1);
	for (let i = 0; i < out.length; i++) energy[i + 1] = energy[i] + out[i] * out[i];
	let best = 0;
	let bestScore = -Infinity;
	for (let lag = lo * MS; lag <= hi * MS; lag++) {
		const t = a + lag;
		if (t < 0 || t + len > out.length) continue;
		const e = energy[t + len] - energy[t];
		if (e <= 0) continue;
		const score = yr[t] / Math.sqrt(e);
		if (score > bestScore) {
			bestScore = score;
			best = lag;
		}
	}
	return best / MS;
}

function onset(env: Float32Array, from: number, to: number): number | null {
	for (let k = Math.max(0, from); k < Math.min(env.length, to); k++) if (env[k] > THRESH) return k;
	return null;
}

function rmsDb(pcm: Float32Array, a: number, b: number): number {
	let s = 0;
	for (let i = a; i < b; i++) s += (pcm[i] ?? 0) ** 2;
	return 10 * Math.log10(s / Math.max(1, b - a) + 1e-20);
}

function writeWav(path: string, pcm: Float32Array) {
	const n = input.length;
	const buf = Buffer.alloc(44 + n * 2);
	buf.write("RIFF", 0);
	buf.writeUInt32LE(36 + n * 2, 4);
	buf.write("WAVEfmt ", 8);
	buf.writeUInt32LE(16, 16);
	buf.writeUInt16LE(1, 20);
	buf.writeUInt16LE(1, 22);
	buf.writeUInt32LE(RATE, 24);
	buf.writeUInt32LE(RATE * 2, 28);
	buf.writeUInt16LE(2, 32);
	buf.writeUInt16LE(16, 34);
	buf.write("data", 36);
	buf.writeUInt32LE(n * 2, 40);
	for (let i = 0; i < n; i++) buf.writeInt16LE(Math.max(-32768, Math.min(32767, Math.round(pcm[i] * 32767))), 44 + i * 2);
	writeFileSync(path, buf);
}

const median = (v: number[]) => [...v].sort((a, b) => a - b)[Math.floor(v.length / 2)];
const rows: Record<string, unknown>[] = [];
for (const file of files) {
	const r = JSON.parse(readFileSync(file, "utf8"));
	if (r.error) console.error(`${r.browser}: ${r.error}`);
	for (const [route, run] of Object.entries(r.paced) as [string, any][]) {
		const stamped = run.packets.filter((p: { ts: number | null }) => p.ts !== null);
		const out = decode(stamped);
		writeWav(`${wavDir}/${r.browser}-${route}.wav`, out);
		const envOut = envelope(out);

		// Codec delay from the first tone; every later lag is drift relative to it.
		// The first tone starts at sample 0, so its output onset is the codec delay.
		const lag0 = align(out, active[0], 0, 50);
		const lags = active.map((seg, i) => (i === 0 ? lag0 : align(out, seg, -9000, 1000)));
		const drift = lags.map((l) => +(l - lag0).toFixed(2));
		// Head: how late the aligned output crosses the onset threshold, per active segment.
		const heads = active.map((seg, i) => {
			const lag = Math.round(lags[i]);
			const a = Math.floor(seg.start / MS) - 5;
			const inOn = onset(envIn, a, a + 400);
			const outOn = inOn === null ? null : onset(envOut, inOn + lag - 50, inOn + lag + 400);
			return inOn === null || outOn === null ? null : outOn - lag - inOn;
		});
		// Speech frames lost: input frames above the gate threshold whose aligned output is 20 dB down.
		let lost = 0;
		let loud = 0;
		active.forEach((seg, i) => {
			if (seg.kind !== "speech") return;
			const shift = Math.round(lags[i] * MS);
			for (let f = seg.start; f + FRAME <= seg.end; f += FRAME) {
				const db = rmsDb(input, f, f + FRAME);
				if (db < -45) continue;
				loud++;
				if (rmsDb(out, f + shift, f + shift + FRAME) < db - 20) lost++;
			}
		});
		const inSilence = (ts: number, kind?: string) => {
			const at = ((ts - BASE_US) / 1e6) * RATE;
			return silent.some((s) => (kind === undefined || s.kind === kind) && at >= s.start && at < s.end);
		};
		const pps = (kind: string) => {
			const sec = silent.filter((s) => s.kind === kind).reduce((t, s) => t + (s.end - s.start), 0) / RATE;
			return +(stamped.filter((p: { ts: number }) => inSilence(p.ts, kind)).length / sec).toFixed(2);
		};
		const silenceSec = silent.reduce((t, s) => t + (s.end - s.start), 0) / RATE;
		const silencePackets = stamped.filter((p: { ts: number }) => inSilence(p.ts));
		// Silence past its first 300 ms, after any hangover: what a long mute costs.
		const steady = (() => {
			const spans = silent.map((s) => ({ start: s.start + 0.3 * RATE, end: s.end }));
			const sec = spans.reduce((t, s) => t + (s.end - s.start), 0) / RATE;
			const hits = stamped.filter((p: { ts: number }) => {
				const at = ((p.ts - BASE_US) / 1e6) * RATE;
				return spans.some((s) => at >= s.start && at < s.end);
			});
			return { pps: +(hits.length / sec).toFixed(2), bps: Math.round((hits.reduce((t: number, p: { len: number }) => t + p.len, 0) * 8) / sec) };
		})();
		const fast = r.fast[route] as { ms: number; encodeMsP99?: number }[];
		const last = stamped.at(-1);
		rows.push({
			browser: r.browser,
			route,
			packets: run.packets.length,
			fed: run.fed,
			unstamped: run.packets.length - stamped.length,
			bytes: run.packets.reduce((t: number, p: { len: number }) => t + p.len, 0),
			silencePps: +(silencePackets.length / silenceSec).toFixed(2),
			silenceBps: Math.round((silencePackets.reduce((t: number, p: { len: number }) => t + p.len, 0) * 8) / silenceSec),
			steadyPps: steady.pps,
			steadyBps: steady.bps,
			zeroPps: pps("silence-zero"),
			roomPps: pps("silence-room"),
			endSec: last ? +((last.ts - BASE_US) / 1e6 + 0.02).toFixed(3) : null,
			codecDelayMs: +lag0.toFixed(2),
			driftMs: drift.slice(1).join("/"),
			headMs: heads.slice(1).map((h) => h ?? "-").join("/"),
			lostSpeechFrames: `${lost}/${loud}`,
			maxLagMs: route === "dtx-native" ? null : +Math.max(...run.packets.map((p: { lagMs: number | null }) => p.lagMs ?? 0)).toFixed(1),
			encodeMsMin: +Math.min(...fast.map((f) => f.ms)).toFixed(1),
			encodeMsMedian: +median(fast.map((f) => f.ms)).toFixed(1),
			encodeP99Ms: fast[0].encodeMsP99 !== undefined ? +median(fast.map((f) => f.encodeMsP99 ?? 0)).toFixed(3) : null,
			rawEndSec: route === "gate" && run.packets.at(-2) ? +((run.packets.at(-2).raw - BASE_US) / 1e6 + 0.02).toFixed(3) : null,
		});
	}
}
console.log(JSON.stringify(rows, null, 1));
const cols = Object.keys(rows[0]);
console.log(cols.join("\t"));
for (const row of rows) console.log(cols.map((c) => row[c] ?? "").join("\t"));
