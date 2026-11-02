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
	return YAML.parse(await Bun.file(`.github/workflows/${name}.yml`).text()) as Workflow;
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

test("tag runs reach their publishers without workflow_run", async () => {
	// winget runs as the tag it submits, dispatched rather than nested so a
	// failed submission leaves the release run, which release-brew needs green.
	const winget = await read("release-winget");
	expect(Object.keys(winget.on)).toEqual(["workflow_dispatch"]);
	expect(winget.jobs.publish.environment).toBe("${{ github.ref_type == 'tag' && 'publish' || '' }}");

	const shared = await read("release-shared-ffi");
	const lib = shared.jobs["kotlin-lib"];
	expect(lib.uses).toBe("./.github/workflows/release-kt-lib.yml");
	expect(lib.needs).toEqual(["kotlin"]);
	expect(lib.if).toContain("github.ref_type == 'tag'");
	expect(lib.if).toContain("needs.kotlin.result == 'success'");
	expect(lib.with).toEqual({ "dry-run": false });
	expect((await read("release-kt-lib")).on.workflow_run).toBeUndefined();
});

test("release dispatches name a release ref", async () => {
	const script = await Bun.file("sh/gh/trigger-repo-publish.sh").text();
	const dispatches = script
		.split("\n")
		.map((line) => line.trim())
		.filter((line) => line.startsWith("gh workflow run"));
	expect(dispatches).toEqual([
		'gh workflow run apt-repo.yml --ref release -f "tag=$TAG"',
		'gh workflow run rpm-repo.yml --ref release -f "tag=$TAG"',
		'gh workflow run release-winget.yml --ref "$TAG"',
		"gh workflow run moq-sh.yml --ref release",
	]);
});
