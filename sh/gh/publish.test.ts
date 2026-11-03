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
	outputs?: Record<string, string>;
	permissions?: Record<string, string>;
	uses?: string;
	needs?: string[];
	environment?: string | { name?: string };
	with?: Record<string, string | boolean>;
	steps?: { id?: string; if?: string }[];
};
type Workflow = {
	on: Record<string, unknown>;
	permissions: Record<string, string>;
	jobs: Record<string, Job>;
};

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
	const kt = await read("release-kt-lib");
	expect(kt.on.workflow_run).toBeUndefined();
	// The call inherits its caller's event, which can be a Nightly dispatched
	// on the tag; its non-dry input, not the event, lets it publish.
	expect(kt.jobs.publish.if).toContain("toJSON(inputs.dry-run) == 'false'");
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
		"gh workflow run moq-sh.yml --ref release",
		'gh workflow run release-winget.yml --ref "$TAG"',
	]);
});

test("Python release retries require release and published, unchanged FFI", async () => {
	const workflow = await read("release-py");
	expect(Object.keys(workflow.on)).toContain("workflow_dispatch");
	const build = workflow.jobs.build;
	const runs = new Function("github", "startsWith", `return (${build.if});`);
	const startsWith = (value: string, prefix: string) => value.startsWith(prefix);
	const publishes = new Function(
		"github",
		"steps",
		`return (${build.outputs?.publish
			.replace(/\$\{\{|\}\}/g, "")
			.replaceAll("steps.ffi-gate", 'steps["ffi-gate"]')
			.replaceAll("steps.ffi-pypi", 'steps["ffi-pypi"]')});`,
	);
	for (const event_name of ["push", "workflow_dispatch"]) {
		const github = {
			event_name,
			ref: "refs/heads/release",
			repository_owner: "moq-dev",
			event: { workflow_run: {} },
		};
		expect(runs(github, startsWith)).toBe(true);
		for (const unreleased of ["true", "false"]) {
			for (const ffiExists of ["true", "false"]) {
				for (const wrapperExists of ["true", "false"]) {
					const steps: Record<string, { outputs: Record<string, string> }> = {
						"ffi-gate": { outputs: {} },
						"ffi-pypi": { outputs: {} },
						pypi: { outputs: { exists: wrapperExists } },
					};
					// A skipped Actions step has empty outputs. Evaluate the real
					// step conditions so a skipped FFI check cannot pass this matrix.
					for (const id of ["ffi-gate", "ffi-pypi"]) {
						const step = build.steps?.find((step) => step.id === id);
						const enabled = new Function(
							"github",
							"steps",
							`return (${step?.if?.replaceAll("steps.ffi-gate", 'steps["ffi-gate"]')});`,
						);
						if (enabled(github, steps))
							steps[id].outputs = id === "ffi-gate" ? { unreleased } : { exists: ffiExists };
					}
					expect([event_name, unreleased, ffiExists, wrapperExists, publishes(github, steps)]).toEqual([
						event_name,
						unreleased,
						ffiExists,
						wrapperExists,
						unreleased === "false" && ffiExists === "true" && wrapperExists === "false",
					]);
				}
			}
		}
	}
	for (const ref of ["refs/heads/main", "refs/heads/topic", "refs/tags/release", "refs/tags/moq-ffi-v0.4.20"]) {
		expect(
			runs(
				{ event_name: "workflow_dispatch", ref, repository_owner: "moq-dev", event: { workflow_run: {} } },
				startsWith,
			),
		).toBe(false);
	}
	const upstream = { conclusion: "success", event: "push", head_branch: "moq-ffi-v0.4.20" };
	for (const [repository_owner, workflow_run, wanted] of [
		["moq-dev", upstream, true],
		["fork", upstream, false],
		["moq-dev", { ...upstream, conclusion: "failure" }, false],
		["moq-dev", { ...upstream, event: "workflow_dispatch" }, false],
		["moq-dev", { ...upstream, head_branch: "main" }, false],
	] as const) {
		expect(
			runs(
				{ event_name: "workflow_run", ref: "refs/heads/main", repository_owner, event: { workflow_run } },
				startsWith,
			),
		).toBe(wanted);
	}
	expect(
		publishes(
			{ event_name: "workflow_run" },
			{ pypi: { outputs: { exists: "false" } }, "ffi-gate": { outputs: {} }, "ffi-pypi": { outputs: {} } },
		),
	).toBe(true);
	expect(workflow.permissions).toEqual({ contents: "read" });
	expect(build.permissions).toBeUndefined();
	expect(build.environment).toBeUndefined();
	expect(workflow.jobs.publish.needs).toEqual(["build"]);
	expect(workflow.jobs.publish.if).toBe("${{ needs.build.outputs.publish == 'true' }}");
	expect(workflow.jobs.publish.environment).toEqual({ name: "pypi", url: "https://pypi.org/p/moq-rs" });
	expect(workflow.jobs.publish.permissions).toEqual({ "id-token": "write" });
});
