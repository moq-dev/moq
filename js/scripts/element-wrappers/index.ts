/**
 * Code generation orchestrator
 *
 * Main entry point for framework wrapper generation.
 * Runs enabled generators in sequence and tracks performance.
 *
 * To add a new framework:
 * 1. Create a new generator in generators/<framework>.ts
 * 2. Export a generate<Framework>Wrappers(basePath) function
 * 3. Add it to the generators array below
 */

import { generateReactWrappers } from "./generators/react";

type GeneratorFunction = (basePath: string) => void;

interface Generator {
	name: string;
	fn: GeneratorFunction;
	enabled: boolean;
}

const generators: Generator[] = [{ name: "React", fn: generateReactWrappers, enabled: true }];

async function generateWrappers(): Promise<void> {
	const startTime = performance.now();

	try {
		console.log("\n📦 Code Generation");

		const basePath = process.cwd();
		const enabledGenerators = generators.filter((g) => g.enabled);

		if (enabledGenerators.length === 0) {
			console.warn("⚠️  No generators enabled");
			return;
		}

		for (const generator of enabledGenerators) {
			try {
				generator.fn(basePath);
			} catch (error) {
				const errorMsg = error instanceof Error ? error.message : String(error);
				console.error(`❌ ${generator.name} failed: ${errorMsg}`);
				throw error;
			}
		}

		const duration = ((performance.now() - startTime) / 1000).toFixed(2);
		console.log(`\n✨ Complete in ${duration}s\n`);
	} catch (error) {
		const errorMsg = error instanceof Error ? error.message : String(error);
		console.error(`\n❌ Generation failed: ${errorMsg}\n`);
		process.exit(1);
	}
}

generateWrappers();
