import { expect, test } from "bun:test";
import { fileURLToPath } from "node:url";

const script = fileURLToPath(new URL("../../../run.sh", import.meta.url));

test("a row whose path changes must play beyond the change", () => {
	for (const profile of ["step", "congestion"]) {
		for (const duration of ["20", "30"]) {
			const result = Bun.spawnSync(["bash", script, "--profiles", profile, "--duration", duration, "--list"]);
			expect(result.exitCode).toBe(2);
			expect(result.stderr.toString()).toContain(`must exceed ${profile}'s change`);
		}
		const result = Bun.spawnSync(["bash", script, "--profiles", profile, "--duration", "31", "--list"]);
		expect(result.exitCode).toBe(0);
		expect(result.stdout.toString()).toContain(`chromium-opus-48000-${profile}`);
	}
});

test("short steady rows remain supported", () => {
	const result = Bun.spawnSync(["bash", script, "--profiles", "mild", "--duration", "20", "--list"]);
	expect(result.exitCode).toBe(0);
});

test("the nightly matrix includes every profile whose path changes", () => {
	const result = Bun.spawnSync(["bash", script, "--list"]);
	expect(result.exitCode).toBe(0);
	const rows = result.stdout
		.toString()
		.trim()
		.split("\n")
		.filter((row) => row.startsWith("chromium-"));
	expect(rows).toHaveLength(14);
	for (const profile of ["bursty", "step", "congestion"]) {
		expect(rows.filter((row) => row.endsWith(`-${profile}`))).toHaveLength(2);
	}
});
