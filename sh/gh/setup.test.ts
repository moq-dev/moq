// Runner timing and action execution stay in CI. These tests check the setup gates.
import { expect, test } from "bun:test";

type Step = {
	name?: string;
	id?: string;
	uses?: string;
	run?: string;
	if?: string;
	"continue-on-error"?: boolean;
};
type Job = { name: string; if?: string; steps: Step[]; "runs-on": string };
const workflow = Bun.YAML.parse(await Bun.file(".github/workflows/check.yml").text()) as {
	on: Record<string, unknown>;
	jobs: Record<string, Job>;
};

for (const name of ["check"]) {
	test(`${name} queries before setup and keeps its required check name`, () => {
		const job = workflow.jobs[name];
		expect(job.name).toBe("Check");
		const scopeIndex = job.steps.findIndex((step) => step.id === "scope");
		expect(scopeIndex).toBeGreaterThan(0);
		const scope = job.steps[scopeIndex];
		expect(scope.if).toBeUndefined();
		expect(scope["continue-on-error"]).toBeUndefined();
		expect(job.steps.slice(0, scopeIndex).some((step) => step.uses === "./.github/actions/just")).toBe(true);

		// No expensive action or command may bypass the query, including future additions.
		// The Rust cache is the largest restore, so it also waits for selected work that compiles Rust.
		const setup = job.steps.slice(scopeIndex + 1);
		expect(setup.some((step) => step.uses === "./.github/actions/nix")).toBe(true);
		expect(setup.some((step) => step.uses === "./.github/actions/rust-cache")).toBe(true);
		expect(setup.filter((step) => step.run).at(-1)?.name).toBe(job.name);
		const checkIndex = setup.findIndex((step) => step.name === job.name);
		for (const [index, step] of setup.entries()) {
			if (step.uses?.startsWith("actions/upload-artifact@")) {
				expect(index).toBeGreaterThan(checkIndex);
				expect(step.if?.replace(/^\$\{\{\s*|\s*\}\}$/g, "")).toBe(
					"!cancelled() && steps.scope.outputs.rust == 'true'",
				);
				continue;
			}
			const output = step.uses === "./.github/actions/rust-cache" ? "rust" : "needed";
			const hosted = step.uses === "./.github/actions/nix" ? " && runner.environment != 'self-hosted'" : "";
			expect(step.if).toBe(`steps.scope.outputs.${output} == 'true'${hosted}`);
		}
	});

	test(`${name} uses the configured runner only for merge groups`, () => {
		const expression = workflow.jobs[name]["runs-on"].replace(/^\$\{\{\s*|\s*\}\}$/g, "");
		const runner = new Function("github", "vars", `return (${expression});`);
		expect(runner({ event_name: "merge_group" }, { MOQ_QUEUE_RUNNER: "moq-queue" })).toBe("moq-queue");
		expect(runner({ event_name: "merge_group" }, {})).toBe("ubuntu-24.04-arm");
		expect(runner({ event_name: "pull_request" }, { MOQ_QUEUE_RUNNER: "moq-queue" })).toBe("ubuntu-24.04-arm");
	});
}

test("self-hosted queue jobs use local caches without R2 credentials", async () => {
	const action = Bun.YAML.parse(await Bun.file(".github/actions/rust-cache/action.yml").text());
	const setup = action.runs.steps.find((step: Step) => step.uses?.startsWith("jdx/mr-boxington-action@"));
	const expression = setup.with.backend
		.replaceAll("inputs.access-key-id", 'inputs["access-key-id"]')
		.replaceAll("inputs.secret-access-key", 'inputs["secret-access-key"]')
		.replace(/^\$\{\{\s*|\s*\}\}$/g, "");
	const backend = new Function("runner", "inputs", `return (${expression});`);
	const guard = new Function("runner", "github", `return (${action.runs.steps[0].if});`);
	for (const event_name of ["merge_group", "pull_request", "pull_request_target", "push", "workflow_dispatch"]) {
		expect(guard({ environment: "self-hosted" }, { event_name })).toBe(event_name !== "merge_group");
		expect(guard({ environment: "github-hosted" }, { event_name })).toBe(false);
	}
	for (const credentials of [
		{ "access-key-id": "key", "secret-access-key": "secret" },
		{ "access-key-id": "", "secret-access-key": "" },
	]) {
		expect(backend({ environment: "self-hosted" }, credentials)).toBe("local");
		expect(backend({ environment: "github-hosted" }, credentials)).toBe(
			credentials["access-key-id"] ? "remote" : "local",
		);
	}
	for (const name of ["Cache credentials", "Check the cache"]) {
		expect(action.runs.steps.find((step: Step) => step.name === name).if).toStartWith(
			"runner.environment != 'self-hosted'",
		);
	}
	expect(action.runs.steps[0].run).toContain("exit 1");
});

test("one job runs one CI dispatch with one checkout and cache restore", () => {
	expect(Object.keys(workflow.jobs)).toEqual(["check"]);
	const steps = workflow.jobs.check.steps;
	expect(steps.filter((step) => step.uses?.startsWith("actions/checkout@"))).toHaveLength(1);
	expect(steps.filter((step) => step.uses === "./.github/actions/nix")).toHaveLength(1);
	expect(steps.filter((step) => step.uses === "./.github/actions/rust-cache")).toHaveLength(1);
	expect(steps.filter((step) => step.run?.includes("nix develop"))).toHaveLength(1);
});
