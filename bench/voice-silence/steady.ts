// Prints which 20 ms blocks of steady white noise the port scores as speech, and the Q8 scores.
import { Vad } from "../../js/publish/src/audio/vad";

let state = 3;
const random = () => {
	state = (Math.imul(state, 1103515245) + 12345) | 0;
	return ((state >>> 8) / 0x800000 - 1) * 0.5;
};
const vad = new Vad({ sampleRate: 48_000, block: 20 });
const scores: number[] = [];
const activity = vad.activity.bind(vad);
vad.activity = (block: Int16Array) => {
	const q8 = activity(block);
	scores.push(q8);
	return q8;
};
const level = Number(process.argv[2] ?? "0.02");
for (let i = 0; i < 3000; i++) vad.push(Float32Array.from({ length: 960 }, () => random() * level));
const active = scores.flatMap((q8, i) => (q8 >= 13 ? [i] : []));
console.log("active blocks:", active.length, active.slice(0, 40).join(" "));
console.log("max score after 5 s:", Math.max(...scores.slice(250)));
