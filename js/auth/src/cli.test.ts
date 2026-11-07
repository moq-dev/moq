import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { chmodSync, mkdtempSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { extname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { wall } from "@moq/clock";

const CLI = join(import.meta.dir, `cli${extname(fileURLToPath(import.meta.url))}`);

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
		dir = mkdtempSync(join(tmpdir(), "moq-auth-cli-"));
	});

	afterEach(() => {
		rmSync(dir, { recursive: true, force: true });
	});

	test("private key is owner-only", () => {
		const key = join(dir, "private.jwk");
		run("generate", "--out", key);
		expect(mode(key)).toBe(0o600);
	});

	test("private key tightens an existing file", async () => {
		const key = join(dir, "existing.jwk");
		writeFileSync(key, "stale");
		chmodSync(key, 0o644);

		run("generate", "--out", key);
		expect(mode(key)).toBe(0o600);

		// The old contents are gone, not just hidden behind the new mode.
		expect(await Bun.file(key).text()).not.toContain("stale");
	});

	test("public key keeps default permissions", () => {
		const key = join(dir, "private.jwk");
		const pub = join(dir, "public.jwk");
		writeFileSync(pub, "");
		chmodSync(pub, 0o644);

		run("generate", "--out", key, "--algorithm", "ES256", "--public", pub);
		expect(mode(pub)).toBe(0o644);
	});
});

describe("sign", () => {
	let dir: string;
	let key: string;

	beforeEach(() => {
		dir = mkdtempSync(join(tmpdir(), "moq-auth-cli-"));
		key = join(dir, "key.jwk");
		run("generate", "--out", key);
	});

	afterEach(() => {
		rmSync(dir, { recursive: true, force: true });
	});

	function exp(...args: string[]): number | undefined {
		const result = Bun.spawnSync(["bun", CLI, "sign", "--key", key, "--publish", "**", ...args]);
		expect(result.exitCode).toBe(0);
		const payload = result.stdout.toString().trim().split(".")[1];
		return JSON.parse(Buffer.from(payload, "base64url").toString()).exp;
	}

	test("writes an hour of exp by default", () => {
		const now = Math.floor(wall() / 1000);
		const value = exp() ?? 0;
		expect(value).toBeGreaterThanOrEqual(now + 3600);
		expect(value).toBeLessThanOrEqual(now + 3602);
	});

	test("--ttl, --expires, and --unbounded pick the exp", () => {
		const now = Math.floor(wall() / 1000);
		expect(exp("--ttl", "1d")).toBeGreaterThanOrEqual(now + 86400);
		expect(exp("--expires", "4102444800")).toBe(4102444800);
		expect(exp("--unbounded")).toBeUndefined();
		const conflict = Bun.spawnSync(["bun", CLI, "sign", "--key", key, "--unbounded", "--ttl", "1d"]);
		expect(conflict.exitCode).not.toBe(0);
	});
});
