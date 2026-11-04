import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { test } from "node:test";
import config from "../../.github/renovate.cjs";
import { GlobalConfig } from "../../.tmp/renovate-cli/node_modules/renovate/dist/config/global.js";
import { init as initLogger } from "../../.tmp/renovate-cli/node_modules/renovate/dist/logger/index.js";
import { updateArtifacts } from "../../.tmp/renovate-cli/node_modules/renovate/dist/modules/manager/cargo/artifacts.js";
import * as git from "../../.tmp/renovate-cli/node_modules/renovate/dist/util/git/index.js";
import { postUpgradeCommandsExecutor } from "../../.tmp/renovate-cli/node_modules/renovate/dist/workers/repository/update/branch/execute-post-upgrade-commands.js";

await initLogger();

const root = path.resolve(import.meta.dirname, "../..");

// Use the pinned updater itself: a schema check cannot catch artifacts that the
// hook regenerates on disk but Renovate then omits from its commit.
test("grouped Cargo updates retain hakari's manifest and lockfile without building", async () => {
	await mkdir(path.join(root, ".tmp"), { recursive: true });
	const scratch = await mkdtemp(path.join(root, ".tmp/renovate-test-"));
	const input = path.join(scratch, "input");
	const output = path.join(scratch, "output");
	const put = async (file, contents) => {
		await mkdir(path.dirname(path.join(input, file)), { recursive: true });
		await writeFile(path.join(input, file), contents);
	};
	await put(".gitignore", "/target\n");
	await put("Cargo.toml", '[workspace]\nmembers = ["a", "b", "rs/workspace-hack"]\nresolver = "2"\n');
	await put(
		".config/hakari.toml",
		'hakari-package = "workspace-hack"\ndep-format-version = "4"\nresolver = "2"\nplatforms = ["x86_64-unknown-linux-gnu"]\n',
	);
	await put(
		"rs/workspace-hack/Cargo.toml",
		'[package]\nname = "workspace-hack"\nversion = "0.1.0"\nedition = "2021"\npublish = false\n### BEGIN HAKARI SECTION\n### END HAKARI SECTION\n',
	);
	await put("rs/workspace-hack/src/lib.rs", "");
	for (const name of ["a", "b"]) {
		await put(
			`${name}/Cargo.toml`,
			`[package]\nname = "${name}"\nversion = "0.1.0"\nedition = "2021"\n[dependencies]\nindexmap = { version = "=1.9.3"${name === "a" ? ', features = ["serde"]' : ""} }\n[dev-dependencies]\nworkspace-hack = { path = "../rs/workspace-hack" }\n`,
		);
		await put(`${name}/src/lib.rs`, "");
		await put(`${name}/build.rs`, 'fn main() { panic!("upgraded code was compiled"); }\n');
	}
	execFileSync("cargo", ["hakari", "generate"], { cwd: input, stdio: "pipe" });
	execFileSync("git", ["init", "--initial-branch=main", "-q"], { cwd: input });
	execFileSync("git", ["add", "."], { cwd: input });
	execFileSync(
		"git",
		[
			"-c",
			"user.name=Fixture",
			"-c",
			"user.email=fixture@example.invalid",
			"-c",
			"commit.gpgsign=false",
			"commit",
			"-qm",
			"fixture",
		],
		{ cwd: input },
	);
	await mkdir(output);
	GlobalConfig.set({ ...config, localDir: output, cacheDir: path.join(scratch, "cache") });
	await git.initRepo({ url: input, defaultBranch: "main" });
	await git.syncGit();
	const updatedPackageFiles = [];
	for (const file of ["a/Cargo.toml", "b/Cargo.toml"]) {
		const contents = (await readFile(path.join(output, file), "utf8")).replace("=1.9.3", "=2.12.1");
		await writeFile(path.join(output, file), contents);
		updatedPackageFiles.push({ type: "addition", path: file, contents });
	}
	const result = await updateArtifacts({
		packageFileName: "a/Cargo.toml",
		newPackageFileContent: updatedPackageFiles[0].contents,
		updatedDeps: [
			{
				depName: "indexmap",
				packageName: "indexmap",
				datasource: "crate",
				currentValue: "=1.9.3",
				newValue: "=2.12.1",
				newVersion: "2.12.1",
				lockedVersion: "1.9.3",
			},
		],
		config: {},
	});
	assert(result?.length);
	assert(!result.some((item) => item.artifactError), JSON.stringify(result));
	const updatedArtifacts = result.map((item) => item.file).filter(Boolean);
	await writeFile(path.join(output, "unrelated.txt"), "must not be committed");
	const branch = { updatedPackageFiles, updatedArtifacts };
	const upgrade = {
		depName: "indexmap",
		packageFile: "a/Cargo.toml",
		postUpgradeTasks: config.cargo.postUpgradeTasks,
	};
	const outcome = await postUpgradeCommandsExecutor([upgrade], branch);
	assert.deepEqual(outcome.artifactErrors, []);
	assert.deepEqual(outcome.updatedArtifacts.map((file) => file.path).sort(), [
		"Cargo.lock",
		"rs/workspace-hack/Cargo.toml",
	]);
	for (const artifact of outcome.updatedArtifacts) {
		assert.equal(artifact.contents.toString(), await readFile(path.join(output, artifact.path), "utf8"));
	}
	execFileSync("cargo", ["hakari", "generate", "--diff"], { cwd: output, stdio: "pipe" });
	execFileSync("cargo", ["metadata", "--locked", "--format-version=1"], { cwd: output, stdio: "pipe" });

	const denied = await postUpgradeCommandsExecutor(
		[{ ...upgrade, postUpgradeTasks: { ...upgrade.postUpgradeTasks, commands: ["cargo build"] } }],
		{ ...branch, updatedArtifacts: outcome.updatedArtifacts },
	);
	assert.equal(denied.artifactErrors.length, 1);
});
