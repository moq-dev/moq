import { expect, test } from "bun:test";
import { fileURLToPath } from "node:url";

test("trace is accepted through the audio-quality wrapper", () => {
	const result = Bun.spawnSync([
		"bash",
		fileURLToPath(new URL("../../run.sh", import.meta.url)),
		"--list",
		"--profiles",
		"near-zero",
		"--codecs",
		"opus",
		"--rings",
		"isolated",
		"--trace",
	]);
	expect(result.exitCode).toBe(0);
	expect(result.stdout.toString().trim()).toBe("chromium-opus-48000-near-zero-isolated");
});

test("the native ring lists native rows and no replays", () => {
	const result = Bun.spawnSync([
		"bash",
		fileURLToPath(new URL("../../run.sh", import.meta.url)),
		"--list",
		"--profiles",
		"mild,relay-mic",
		"--codecs",
		"opus",
		"--rings",
		"pulse",
	]);
	expect(result.exitCode).toBe(0);
	expect(result.stdout.toString().trim()).toBe("native-opus-48000-mild-pulse");
});
