// Fail when a workflow `run:` step names a `.sh` file. Workflows call `just`
// recipes, so a script can move or change its arguments without touching them,
// and the same command works locally.
import { Glob } from "bun";

type Step = { run?: string };
type Job = { steps?: Step[] };
type Workflow = { jobs?: Record<string, Job> };

const violations: string[] = [];
for await (const path of new Glob(".github/workflows/*.{yml,yaml}").scan({ dot: true })) {
	const workflow = Bun.YAML.parse(await Bun.file(path).text()) as Workflow;
	for (const [name, job] of Object.entries(workflow.jobs ?? {})) {
		for (const step of job.steps ?? []) {
			for (const line of step.run?.split("\n") ?? []) {
				if (/\.sh\b/.test(line)) violations.push(`${path} (${name}): ${line.trim()}`);
			}
		}
	}
}

if (violations.length > 0) {
	console.error("workflow steps must run a `just` recipe, not a script:");
	for (const violation of violations.sort()) console.error(`  ${violation}`);
	process.exit(1);
}
