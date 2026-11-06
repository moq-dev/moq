import { expect, test } from "bun:test";
import { ToneCheck } from "./tone";

const wrong = {
	at: 0,
	audioContext: "running",
	audioStalled: false,
	audioCodec: "opus",
	audioChannels: 2,
	tones: [
		{ rms: 0.35, share: 0 },
		{ rms: 0.35, share: 0 },
	],
};
const expected = { codec: "opus", channels: 2 };
const correct = wrong.tones.map(() => ({ rms: 0.35, share: 1 }));

test("a refill pauses rather than resets or consumes the tone grace", () => {
	const check = new ToneCheck(expected);
	expect(check.read(wrong)).toBe("waiting");
	expect(check.read({ ...wrong, at: 1000 })).toBe("waiting");
	expect(check.read({ ...wrong, at: 1100, audioStalled: true })).toBe("waiting");
	expect(check.read({ ...wrong, at: 6100, audioStalled: true })).toBe("waiting");
	expect(check.read({ ...wrong, at: 6200 })).toBe("waiting");
	expect(check.read({ ...wrong, at: 8199 })).toBe("waiting");
	expect(check.read({ ...wrong, at: 8200 })).toBe("failed");
});

test("repeated observations cannot spend playback grace while the page is starved", () => {
	const check = new ToneCheck(expected);
	for (let poll = 0; poll < 100; poll++) expect(check.read(wrong)).toBe("waiting");
	expect(check.read({ ...wrong, at: 2999 })).toBe("waiting");
	expect(check.read({ ...wrong, at: 3000 })).toBe("failed");
});

test("suspension and startup buffering contribute no playback time", () => {
	const check = new ToneCheck(expected);
	expect(check.read({ ...wrong, audioStalled: true })).toBe("waiting");
	expect(check.read({ ...wrong, at: 5000 })).toBe("waiting");
	expect(check.read({ ...wrong, at: 6000 })).toBe("waiting");
	expect(check.read({ ...wrong, at: 6100, audioContext: "suspended" })).toBe("waiting");
	expect(check.read({ ...wrong, at: 16100, audioContext: "suspended" })).toBe("waiting");
	expect(check.read({ ...wrong, at: 16200 })).toBe("waiting");
	expect(check.read({ ...wrong, at: 18200 })).toBe("failed");
});

test("analyser residue while stalled cannot satisfy the tone check", () => {
	const check = new ToneCheck(expected);
	expect(check.read({ ...wrong, audioStalled: true, tones: correct })).toBe("waiting");
	expect(check.read({ ...wrong, at: 10000, tones: correct })).toBe("heard");
});

for (const bad of [
	{ tones: wrong.tones.map(() => ({ rms: 0, share: 1 })) },
	{ audioCodec: "mp4a.40.2", tones: correct },
	{ audioChannels: 1, tones: correct },
	{ tones: correct.slice(0, 1) },
]) {
	test(`playing invalid audio still fails: ${JSON.stringify(bad)}`, () => {
		const check = new ToneCheck(expected);
		expect(check.read({ ...wrong, ...bad })).toBe("waiting");
		expect(check.read({ ...wrong, ...bad, at: 3000 })).toBe("failed");
	});
}
