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
			const expected =
				!["pull_request", "pull_request_target"].includes(event) &&
				["failure", "timed_out"].includes(conclusion);
			for (const name of ["Release JS", "moq.sh", "release-brew", "apt-repo"]) {
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

test("ordinary CI never subscribes to Discord alerts", () => {
	for (const name of ["Check", "Cache", "Land", "OBS", "Platform", "Quest"]) {
		expect(watched).not.toContain(name);
	}
});

test("alert coverage excludes queue-only workflows and retains release and nightly workflows", () => {
	const root = mkdtempSync(join(tmpdir(), "moq-alert-"));
	try {
		mkdirSync(join(root, "sh/gh"), { recursive: true });
		mkdirSync(join(root, ".github/workflows"), { recursive: true });
		copyFileSync("sh/gh/alert.sh", join(root, "sh/gh/alert.sh"));
		const configs = {
			alert: { name: "Alert", on: { workflow_run: { workflows: ["Release", "Nightly"] } } },
			check: { name: "Check", on: { pull_request: null } },
			land: { name: "Land", on: { pull_request_target: null, workflow_dispatch: null, push: null } },
			quest: { name: "Quest", on: { push: { branches: ["quest/**"] } } },
			cache: { name: "Cache", on: { push: { branches: ["main"] } } },
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
