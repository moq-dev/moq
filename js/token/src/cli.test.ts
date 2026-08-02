import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { chmodSync, mkdtempSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const CLI = join(import.meta.dir, "cli.ts");

function run(...args: string[]) {
	const result = Bun.spawnSync(["bun", CLI, ...args]);
	expect(result.stderr.toString()).toBe("");
	expect(result.exitCode).toBe(0);
}

function mode(path: string): number {
	return statSync(path).mode & 0o777;
}

// The mode bits don't exist on Windows, where the file inherits the directory's ACL instead.
describe.skipIf(process.platform === "win32")("generate permissions", () => {
	let dir: string;

	beforeEach(() => {
		dir = mkdtempSync(join(tmpdir(), "moq-token-cli-"));
	});

	afterEach(() => {
		rmSync(dir, { recursive: true, force: true });
	});

	test("private key is owner-only", () => {
		const key = join(dir, "private.jwk");
		run("generate", "--key", key);
		expect(mode(key)).toBe(0o600);
	});

	test("private key tightens an existing file", async () => {
		const key = join(dir, "existing.jwk");
		writeFileSync(key, "stale");
		chmodSync(key, 0o644);

		run("generate", "--key", key);
		expect(mode(key)).toBe(0o600);

		// The old contents are gone, not just hidden behind the new mode.
		expect(await Bun.file(key).text()).not.toContain("stale");
	});

	test("public key keeps default permissions", () => {
		const key = join(dir, "private.jwk");
		const pub = join(dir, "public.jwk");
		writeFileSync(pub, "");
		chmodSync(pub, 0o644);

		run("generate", "--key", key, "--algorithm", "ES256", "--public", pub);
		expect(mode(pub)).toBe(0o644);
	});
});
