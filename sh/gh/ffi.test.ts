// biome-ignore-all lint/suspicious/noTemplateCurlyInString: GitHub Actions expressions are literal strings.
import { expect, test } from "bun:test";
import { YAML } from "bun";

type Job = {
	name?: string;
	if?: string;
	uses?: string;
	needs?: string[];
	permissions?: Record<string, string>;
	with?: Record<string, string | boolean>;
	steps?: { name?: string; if?: string; env?: Record<string, string>; with?: Record<string, string>; run?: string }[];
};
type Workflow = {
	on: { push?: { tags?: string[] }; workflow_call?: unknown; workflow_run?: { workflows: string[] } };
	permissions: Record<string, string>;
	jobs: Record<string, Job>;
};
async function read(name: string): Promise<Workflow> {
	return YAML.parse(await Bun.file(`.github/workflows/${name}.yml`).text()) as Workflow;
}

test("shared default builds cover each consumer exactly once", async () => {
	const shared = await read("release-shared-ffi");
	const targets = JSON.parse(shared.jobs.native.with?.targets as string) as string[];
	expect(new Set(targets).size).toBe(targets.length);
	const all: string[] = [];
	for (const [job, file] of [
		["go", "go"],
		["kotlin", "kt"],
		["swift", "swift"],
	]) {
		const consumer = await read(`release-${file}-ffi`);
		all.push(...JSON.parse(consumer.jobs.ffi.with?.targets as string));
		expect(consumer.on.push).toBeUndefined();
		expect(consumer.on.workflow_call).toBeDefined();
		expect(shared.jobs[job].with?.["reuse-native"]).toBe(true);
		expect(shared.jobs[job].if).toContain("always()");
		expect(shared.jobs[job].if).toContain("!cancelled()");
		const select = consumer.jobs.package.steps?.find((step) => step.name === "Select native artifacts");
		expect(select?.env?.BUILD_TARGETS).toBe("${{ needs.ffi.outputs.targets }}");
		expect(select?.env?.LIB_PREFIX).toContain("'default'");
	}
	expect([...new Set(all)].sort()).toEqual(targets.toSorted());
	expect(shared.on.push?.tags).toEqual(["moq-ffi-v*"]);
	const dart = await read("release-dart-ffi");
	expect(dart.on.push?.tags).toEqual(["moq-ffi-v*"]);
});

test("wrapper gates preserve trusted tag inputs and inspect only their publisher", async () => {
	for (const [file, language, first] of [
		["go", "go", "build"],
		["kt-lib", "kotlin", "check-version"],
		["swift-lib", "swift", "build"],
	]) {
		const wrapper = await read(`release-${file}`);
		expect(wrapper.on.workflow_run?.workflows).toContain("Release FFI");
		expect(wrapper.permissions).toEqual({ contents: "read" });
		const gate = wrapper.jobs["ffi-release"];
		expect(gate.permissions).toEqual({ contents: "read", actions: "read" });
		expect(gate.if).toContain("github.repository_owner == 'moq-dev'");
		expect(gate.if).toContain("github.event.workflow_run.event == 'push'");
		expect(gate.if).toContain("startsWith(github.event.workflow_run.head_branch, 'moq-ffi-v')");
		expect(gate.if).toContain("github.event.workflow_run.name == 'Release FFI'");
		expect(gate.steps?.[0]?.with?.ref).toBe("${{ github.sha }}");
		expect(gate.steps?.at(-1)?.run).toBe(`just gh ffi completed ${language}`);
		expect(gate.steps?.at(-1)?.env?.UPSTREAM_RUN_STARTED_AT).toBe(
			"${{ github.event.workflow_run.run_started_at }}",
		);
		expect(wrapper.jobs[first].needs).toEqual(["ffi-release"]);
		expect(wrapper.jobs[first].if).toBe("needs.ffi-release.outputs.ready == 'true'");
	}
});

// The tag patterns the `release tags` ruleset protects (infra/README.md).
const PROTECTED = /^moq-(relay|cli|boy|c|gst|ffi|dart)-v$/;

test("privileged workflow_run jobs require a protected release tag before running its code", async () => {
	for (const [file, job] of [
		["release-go", "ffi-release"],
		["release-kt-lib", "ffi-release"],
		["release-swift-lib", "ffi-release"],
		["release-py", "build"],
		["release-brew", "publish"],
		["release-winget", "publish"],
	]) {
		const gated = (await read(file)).jobs[job];
		// Every accepted head_branch prefix must be one the `release tags` ruleset
		// protects, or a pusher could create a matching tag on their own commit.
		const prefixes = [...(gated.if ?? "").matchAll(/head_branch, '([^']+)'\)/g)].map((match) => match[1]);
		expect([file, prefixes.length > 0]).toEqual([file, true]);
		for (const prefix of prefixes) expect([file, prefix]).toEqual([file, expect.stringMatching(PROTECTED)]);
		const steps = gated.steps ?? [];
		const gate = steps.findIndex((step) => step.name === "Require a release tag at the run's commit");
		expect([file, gate]).not.toEqual([file, -1]);
		expect(steps[gate].env).toEqual({
			GH_TOKEN: "${{ github.token }}",
			TAG: "${{ github.event.workflow_run.head_branch }}",
			SHA: "${{ github.event.workflow_run.head_sha }}",
		});
		expect(steps[gate].run).toContain('commits/refs/tags/$TAG"');
		// Nothing from the triggering commit runs before the gate.
		const head = steps.findIndex((step) => step.with?.ref?.includes("workflow_run.head_sha"));
		expect([file, head === -1 || head > gate]).toEqual([file, true]);
	}
});

test("Nightly and alert chains retain packaged outputs with one shared producer", async () => {
	const nightly = await read("nightly");
	expect(nightly.jobs["release-shared-ffi"].uses).toBe("./.github/workflows/release-shared-ffi.yml");
	expect(nightly.jobs.sizes.needs).toContain("release-shared-ffi");
	expect(nightly.jobs["release-py-ffi"]).toBeDefined();
	expect(nightly.jobs["release-dart-ffi"]).toBeDefined();
	// A called workflow's jobs cannot exceed the caller's grant, so a dry-run
	// of a gated wrapper must pass on the gate's permissions.
	for (const [name, job] of Object.entries(nightly.jobs)) {
		const file = job.uses?.match(/^\.\/\.github\/workflows\/(.+)\.yml$/)?.[1];
		const gate = file ? (await read(file)).jobs["ffi-release"] : undefined;
		if (!gate) continue;
		for (const [scope, level] of Object.entries(gate.permissions ?? {}))
			expect([name, scope, job.permissions?.[scope]]).toEqual([name, scope, level]);
	}
	expect((await read("alert")).on.workflow_run?.workflows).toContain("Release FFI");
});
