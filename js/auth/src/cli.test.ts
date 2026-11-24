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

	function payload(...args: string[]) {
		const result = Bun.spawnSync(["bun", CLI, "sign", "--key", key, "--publish", "", ...args]);
		expect(result.exitCode).toBe(0);
		const encoded = result.stdout.toString().trim().split(".")[1];
		return JSON.parse(Buffer.from(encoded, "base64url").toString());
	}

	function exp(...args: string[]): number | undefined {
		return payload(...args).exp;
	}

	/** The `exp` `args` write, with the wall seconds bracketing the subprocess that wrote it. */
	function bracketed(...args: string[]) {
		const before = Math.floor(wall().asUnixMillis() / 1000);
		const value = exp(...args) ?? 0;
		const after = Math.floor(wall().asUnixMillis() / 1000);
		return { before, value, after };
	}

	test("writes an hour of exp by default", () => {
		const { before, value, after } = bracketed();
		expect(value).toBeGreaterThanOrEqual(before + 3600);
		expect(value).toBeLessThanOrEqual(after + 3600);
	});

	test("--ttl, --expires, and --unbounded pick the exp", () => {
		const { before, value, after } = bracketed("--ttl", "1d");
		expect(value).toBeGreaterThanOrEqual(before + 86400);
		expect(value).toBeLessThanOrEqual(after + 86400);
		expect(exp("--expires", "4102444800")).toBe(4102444800);
		expect(exp("--unbounded")).toBeUndefined();
		const conflict = Bun.spawnSync(["bun", CLI, "sign", "--key", key, "--unbounded", "--ttl", "1d"]);
		expect(conflict.exitCode).not.toBe(0);
	});

	test("writes a jti only when --id gives one", () => {
		expect(payload()).not.toHaveProperty("jti");
		expect(payload("--id", "t-1").jti).toBe("t-1");
		const invalid = Bun.spawnSync(["bun", CLI, "sign", "--key", key, "--publish", "", "--id", "a b"]);
		expect(invalid.exitCode).not.toBe(0);
	});
});

describe("grants", () => {
	let dir: string;

	beforeEach(() => {
		dir = mkdtempSync(join(tmpdir(), "moq-auth-cli-"));
	});

	afterEach(() => {
		rmSync(dir, { recursive: true, force: true });
	});

	test("flags take prefixes alone", () => {
		const key = join(dir, "key.jwk");
		run("generate", "--out", key);
		for (const args of [
			["generate", "--out", join(dir, "scoped.jwk"), "--publish", "room/**"],
			["generate", "--out", join(dir, "scoped.jwk"), "--subscribe", "**"],
			["sign", "--key", key, "--publish", "room/**"],
			["sign", "--key", key, "--subscribe", "**"],
		]) {
			const result = Bun.spawnSync(["bun", CLI, ...args]);
			expect(result.exitCode, args.join(" ")).not.toBe(0);
			expect(result.stderr.toString(), args.join(" ")).toContain("wildcard");
		}
	});

	// TEMPORARY MIGRATION (see `readPrefix` in wire.ts): the scope a release before 0.22 reads.
	test("a key scope is written as the subtree patterns every release reads", async () => {
		const key = join(dir, "scoped.jwk");
		run("generate", "--out", key, "--root", "demo", "--publish", "room", "--subscribe", "");
		expect(JSON.parse(await Bun.file(key).text()).scope).toEqual({
			root: "demo",
			publish: ["room/**"],
			subscribe: ["**"],
		});
	});
});
