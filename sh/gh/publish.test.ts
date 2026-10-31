// biome-ignore-all lint/suspicious/noTemplateCurlyInString: GitHub Actions expressions are literal strings.
import { expect, test } from "bun:test";
import { readdir } from "node:fs/promises";
import { YAML } from "bun";

// The `publish` Environment admits only `release` and the release tags
// (infra/README.md, "Write credentials"). GitHub runs workflow_run, schedule,
// and a dispatch without --ref as `main`, so a job naming `publish` there
// stops starting once `main` leaves its deployment policy.

type Job = {
	if?: string;
	uses?: string;
	needs?: string[];
	environment?: string | { name?: string };
	with?: Record<string, string | boolean>;
};
type Workflow = { on: Record<string, unknown>; jobs: Record<string, Job> };

async function read(name: string): Promise<Workflow> {
	const doc = YAML.parse(await Bun.file(`.github/workflows/${name}.yml`).text()) as Workflow & { true?: unknown };
	// Bun parses a bare `on:` key as boolean true (YAML 1.1).
	return { ...doc, on: (doc.on ?? doc.true) as Record<string, unknown> };
}

function namesPublish(job: Job): boolean {
	const env = typeof job.environment === "object" ? job.environment?.name : job.environment;
	return env === "publish" || (env ?? "").includes("'publish'");
}

test("no workflow that runs as main names the publish Environment", async () => {
	for (const file of await readdir(".github/workflows")) {
		if (!file.endsWith(".yml")) continue;
		const name = file.replace(/\.yml$/, "");
		const workflow = await read(name);
		const triggers = typeof workflow.on === "object" && workflow.on !== null ? Object.keys(workflow.on) : [];
		if (!triggers.some((trigger) => trigger === "workflow_run" || trigger === "schedule")) continue;
		for (const [id, job] of Object.entries(workflow.jobs)) {
			expect([`${name}/${id}`, namesPublish(job)]).toEqual([`${name}/${id}`, false]);
		}
	}
});

test("tag runs call their publishers instead of chaining them by workflow_run", async () => {
	const binary = await read("release-binary");
	expect(binary.jobs.winget.uses).toBe("./.github/workflows/release-winget.yml");
	expect(binary.jobs.winget.needs).toEqual(["release"]);
	expect(binary.jobs.winget.if).toBe("github.ref_type == 'tag'");
	expect(binary.jobs.winget.with).toEqual({ submit: true });

	const winget = await read("release-winget");
	expect(Object.keys(winget.on).sort()).toEqual(["workflow_call", "workflow_dispatch"]);
	expect(winget.jobs.publish.environment).toBe("${{ inputs.submit && 'publish' || '' }}");
	expect(winget.jobs.publish.if).toContain("inputs.submit && github.ref_type == 'tag'");

	const shared = await read("release-shared-ffi");
	const lib = shared.jobs["kotlin-lib"];
	expect(lib.uses).toBe("./.github/workflows/release-kt-lib.yml");
	expect(lib.needs).toEqual(["kotlin"]);
	expect(lib.if).toContain("github.ref_type == 'tag'");
	expect(lib.if).toContain("needs.kotlin.result == 'success'");
	expect(lib.with).toEqual({ "dry-run": false });
	expect((await read("release-kt-lib")).on.workflow_run).toBeUndefined();
});

test("release dispatches name the release branch", async () => {
	const script = await Bun.file("sh/gh/trigger-repo-publish.sh").text();
	const dispatches = script.split("\n").filter((line) => line.trim().startsWith("gh workflow run"));
	expect(dispatches.length).toBe(3);
	for (const line of dispatches) expect(line).toContain("--ref release");
});
