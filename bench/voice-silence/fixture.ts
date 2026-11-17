// Writes analysis.test.ts's signal as s16le mono PCM, for regenerating its libopus vectors.
// Usage: bun fixture.ts <rate> <out.s16>
export function signal(rate: number): Float32Array {
	let state = 3;
	const random = () => {
		state = (Math.imul(state, 1103515245) + 12345) | 0;
		return (state >>> 8) / 0x1000000 - 0.5;
	};
	// A one-pole low-pass, for the fan's rumble.
	let fan = 0;
	let phase = 0;
	return Float32Array.from({ length: rate * 5 }, (_, n) => {
		const t = n / rate;
		const room = random() * 0.001;
		fan += 0.05 * (random() - fan);
		const rumble = t >= 2 ? fan * 0.07 : 0;
		// A voiced talker: harmonics of a gliding pitch, in syllables.
		phase += (2 * Math.PI * (130 + 25 * Math.sin(2 * Math.PI * 1.7 * t))) / rate;
		const syllable = Math.max(0, Math.sin(2 * Math.PI * 2.5 * t)) ** 2;
		let voice = 0;
		for (let h = 1; h <= 20; h++) voice += Math.sin(h * phase) / h;
		const talking = t < 1 || t >= 4;
		return Math.round((room + rumble + (talking ? 0.1 * syllable * voice : 0)) * 32767) / 32768;
	});
}

if (import.meta.main) {
	const [rate, out] = process.argv.slice(2);
	const samples = signal(Number(rate));
	await Bun.write(out, Int16Array.from(samples, (x) => Math.round(x * 32768)));
}
