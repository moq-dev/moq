import { expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

test("worker checks select changed projects and propagate build failures", () => {
	const temp = mkdtempSync(join(tmpdir(), "moq-workers-"));
	const log = join(temp, "calls");
	writeFileSync(
		join(temp, "bun"),
		'#!/bin/sh\nprintf "%s %s\\n" "$PWD" "$*" >> "$WORKER_TEST_LOG"\nif [ "$*" = "run deploy --dry-run" ]; then exit "$WORKER_TEST_EXIT"; fi\n',
		{ mode: 0o755 },
	);
	try {
		for (const project of ["infra/apt", "infra/rpm", "demo/pub"]) {
			writeFileSync(log, "");
			const result = Bun.spawnSync(["just", "js", "check", `${project}/bun.lock`], {
				cwd: resolve(import.meta.dir, "../.."),
				env: {
					...process.env,
					PATH: `${temp}:${process.env.PATH}`,
					WORKER_TEST_LOG: log,
					WORKER_TEST_EXIT: "0",
				},
			});
			expect(result.exitCode).toBe(0);
			expect(readFileSync(log, "utf8").trim().split("\n")).toEqual([
				`${resolve(import.meta.dir, "../..", project)} install --frozen-lockfile`,
				`${resolve(import.meta.dir, "../..", project)} run deploy --dry-run`,
			]);
		}
		const failed = Bun.spawnSync(["just", "js", "check", "infra/apt/bun.lock"], {
			cwd: resolve(import.meta.dir, "../.."),
			env: {
				...process.env,
				PATH: `${temp}:${process.env.PATH}`,
				WORKER_TEST_LOG: log,
				WORKER_TEST_EXIT: "1",
			},
		});
		expect(failed.exitCode).not.toBe(0);
	} finally {
		rmSync(temp, { recursive: true, force: true });
	}
});
