import { expect, test } from "bun:test";
import { fileURLToPath } from "node:url";

const script = fileURLToPath(new URL("../../../run.sh", import.meta.url));

test("a step row must play beyond the profile change", () => {
	for (const duration of ["20", "30"]) {
		const result = Bun.spawnSync(["bash", script, "--profiles", "step", "--duration", duration, "--list"]);
		expect(result.exitCode).toBe(2);
		expect(result.stderr.toString()).toContain("must exceed the step at 30s");
	}
	const result = Bun.spawnSync(["bash", script, "--profiles", "step", "--duration", "31", "--list"]);
	expect(result.exitCode).toBe(0);
	expect(result.stdout.toString()).toContain("chromium-opus-48000-step-plain");
});

test("short steady rows remain supported", () => {
	const result = Bun.spawnSync(["bash", script, "--profiles", "mild", "--duration", "20", "--list"]);
	expect(result.exitCode).toBe(0);
});

test("the nightly matrix includes both new profiles", () => {
	const result = Bun.spawnSync(["bash", script, "--list"]);
	expect(result.exitCode).toBe(0);
	const rows = result.stdout.toString().trim().split("\n");
	expect(rows).toHaveLength(24);
	for (const profile of ["bursty", "step"]) {
		expect(rows.filter((row) => row.includes(`-${profile}-`))).toHaveLength(4);
	}
});
