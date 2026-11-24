import { expect, test } from "bun:test";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const workflow = Bun.YAML.parse(await Bun.file(".github/workflows/alert.yml").text());
const evaluate = new Function("github", "contains", "fromJSON", `return (${workflow.jobs.discord.if});`);
const watched = workflow.on.workflow_run.workflows as string[];
const condition = (run: { name: string; event: string; conclusion: string }) =>
	watched.includes(run.name) &&
	evaluate({ event: { workflow_run: run } }, (values: string[], value: string) => values.includes(value), JSON.parse);

for (const event of ["pull_request", "pull_request_target", "push", "schedule", "workflow_dispatch", "workflow_run"]) {
	test(`Discord alert policy for ${event}`, () => {
		for (const conclusion of ["failure", "timed_out", "success", "cancelled", "skipped"]) {
			for (const name of ["Release JS", "moq.sh", "release-brew", "apt-repo", "rpm-repo"]) {
				// `queue: max` never cancels a package-repo publish, so a cancelled one is a dropped release.
				const queued = ["release-brew", "apt-repo", "rpm-repo"].includes(name);
				const expected =
					!["pull_request", "pull_request_target"].includes(event) &&
					(["failure", "timed_out"].includes(conclusion) || (queued && conclusion === "cancelled"));
				expect(condition({ name, event, conclusion })).toBe(expected);
			}
		}
	});
}

test("cron workflows only alert on scheduled failures", async () => {
	for await (const file of new Bun.Glob(".github/workflows/*.yml").scan()) {
		const config = Bun.YAML.parse(await Bun.file(file).text());
		if (!config.on?.schedule) continue;
		for (const event of ["schedule", "workflow_dispatch", "pull_request"]) {
			for (const conclusion of ["failure", "timed_out", "success", "cancelled", "skipped"]) {
				expect(condition({ name: config.name, event, conclusion })).toBe(
					event === "schedule" && ["failure", "timed_out"].includes(conclusion),
				);
			}
		}
	}
});

test("CodSpeed alerts on failed landings, not on bisect dispatches", () => {
	for (const event of ["push", "workflow_dispatch", "pull_request"]) {
		for (const conclusion of ["failure", "timed_out", "success", "cancelled"]) {
			expect(condition({ name: "CodSpeed", event, conclusion })).toBe(
				event === "push" && ["failure", "timed_out"].includes(conclusion),
			);
		}
	}
});

test("ordinary CI never subscribes to Discord alerts", () => {
	for (const name of ["Check", "OBS", "Platform", "Quest"]) {
		expect(watched).not.toContain(name);
	}
});

test("alert coverage excludes CI-only workflows and retains release and nightly workflows", () => {
	const root = mkdtempSync(join(tmpdir(), "moq-alert-"));
	try {
		mkdirSync(join(root, "sh/gh"), { recursive: true });
		mkdirSync(join(root, ".github/workflows"), { recursive: true });
		copyFileSync("sh/gh/alert.sh", join(root, "sh/gh/alert.sh"));
		const configs = {
			alert: { name: "Alert", on: { workflow_run: { workflows: ["Release", "Nightly"] } } },
			check: { name: "Check", on: { pull_request: null } },
			quest: { name: "Quest", on: { push: { branches: ["quest/**"] } } },
			obs: { name: "OBS", on: { push: { branches: ["main"] } } },
			release: { name: "Release", on: { pull_request: null, push: null } },
			nightly: { name: "Nightly", on: "schedule" },
		};
		for (const [name, config] of Object.entries(configs)) {
			writeFileSync(join(root, `.github/workflows/${name}.yml`), Bun.YAML.stringify(config));
		}
		const result = Bun.spawnSync(["bash", "sh/gh/alert.sh", "check-coverage"], { cwd: root });
		expect(result.stderr.toString()).toBe("");
		expect(result.exitCode).toBe(0);
		expect(result.stdout.toString()).toContain("watches all 2");
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});

test("a landing failure streak posts once a day", () => {
	const root = mkdtempSync(join(tmpdir(), "moq-alert-"));
	try {
		mkdirSync(join(root, "bin"));
		// `gh` prints the runs file, or fails when it is missing; `curl` records each post.
		writeFileSync(join(root, "bin/gh"), `#!/usr/bin/env bash\nexec cat "${root}/runs.json"\n`, { mode: 0o755 });
		writeFileSync(join(root, "bin/curl"), `#!/usr/bin/env bash\necho post >> "${root}/posts"\n`, { mode: 0o755 });
		// Earlier runs as [run_number, conclusion, created_at], today's unless given.
		const post = (once: boolean, runs?: [number, string, string?][], createdAt = "2026-11-23T00:00:05Z") => {
			rmSync(join(root, "posts"), { force: true });
			rmSync(join(root, "runs.json"), { force: true });
			if (runs) {
				const workflow_runs = runs.map(([run_number, conclusion, created_at = "2026-11-23T00:00:01Z"]) => ({
					run_number,
					conclusion,
					created_at,
				}));
				writeFileSync(join(root, "runs.json"), JSON.stringify({ workflow_runs }));
			}
			const result = Bun.spawnSync(["bash", "sh/gh/alert.sh", "discord"], {
				env: {
					...process.env,
					PATH: `${join(root, "bin")}:${process.env.PATH}`,
					DISCORD_WEBHOOK: "https://discord.invalid/webhook",
					ALERT_ONCE: String(once),
					GITHUB_REPOSITORY: "moq-dev/moq",
					RUN_WORKFLOW_ID: "7",
					RUN_NUMBER: "10",
					RUN_CREATED_AT: createdAt,
					RUN_NAME: "CodSpeed",
					RUN_EVENT: "push",
					RUN_REF: "main",
				},
			});
			expect(result.exitCode).toBe(0);
			return Bun.file(join(root, "posts")).size > 0;
		};

		// The latest earlier verdict decides; cancelled (coalesced) runs and later runs are passed over.
		expect(post(true, [[9, "failure"]])).toBe(false);
		expect(post(true, [[9, "timed_out"]])).toBe(false);
		expect(post(true, [[9, "success"]])).toBe(true);
		expect(
			post(true, [
				[11, "success"],
				[9, "cancelled"],
				[8, "skipped"],
				[7, "failure"],
				[6, "success"],
			]),
		).toBe(false);
		expect(
			post(true, [
				[11, "failure"],
				[9, "cancelled"],
				[7, "success"],
				[6, "failure"],
			]),
		).toBe(true);
		// Unsorted pages still pick the latest earlier run.
		expect(
			post(true, [
				[3, "failure"],
				[8, "success"],
			]),
		).toBe(true);
		// A streak posts its first failure of each UTC day, so a lost post costs at most a day.
		expect(post(true, [[9, "failure", "2026-11-22T23:59:59Z"]])).toBe(true);
		expect(post(true, [[9, "timed_out", "2026-11-20T12:00:00Z"]])).toBe(true);
		expect(post(true, [[9, "failure", "2026-11-23T23:59:59Z"]], "2026-11-23T23:59:59Z")).toBe(false);
		// Only the latest earlier verdict's day counts, not an older failure's.
		expect(
			post(true, [
				[9, "failure", "2026-11-22T20:00:00Z"],
				[8, "failure", "2026-11-23T00:00:00Z"],
			]),
		).toBe(true);
		// No earlier verdict, no way to read one, or no day to compare posts rather than risk a lost alert.
		expect(post(true, [])).toBe(true);
		expect(post(true)).toBe(true);
		expect(post(true, [[9, "failure"]], "")).toBe(true);
		// Without ALERT_ONCE every failure posts, and earlier runs are never read.
		expect(post(false, [[9, "failure"]])).toBe(true);
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});
