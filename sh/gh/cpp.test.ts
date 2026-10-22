import { expect, test } from "bun:test";
import { YAML } from "bun";

test("OBS builds on branch dry runs, and on tags only after the C++ release succeeds", async () => {
	const workflow = YAML.parse(await Bun.file(".github/workflows/release-cpp.yml").text());
	const job = workflow.jobs["obs-build"];
	const runs = new Function("github", "needs", "always", "cancelled", `return (${job.if.replace(/\$\{\{|\}\}/g, "")});`);
	for (const [ref, build, release, wanted] of [
		["branch", "success", "skipped", true],
		["tag", "success", "success", true],
		["tag", "success", "failure", false],
		["branch", "failure", "skipped", false],
	] as const) {
		expect(runs({ ref_type: ref }, { build: { result: build }, release: { result: release } }, () => true, () => false)).toBe(
			wanted,
		);
	}
});
