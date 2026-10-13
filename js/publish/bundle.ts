import assert from "node:assert/strict";
import type { Plugin } from "vite";

// Check the actual release graph during its build, outside unit-test execution limits.
export function lazyMedia(): Plugin {
	return {
		name: "lazy-media",
		generateBundle(_options, bundle) {
			const chunks = new Map(
				Object.values(bundle)
					.filter((output) => output.type === "chunk")
					.map((chunk) => [chunk.fileName, chunk]),
			);
			const media = [...chunks.values()].filter((chunk) =>
				Object.keys(chunk.modules).some((id) => id.includes("/mediabunny/")),
			);
			assert(media.length > 0, "Expected a mediabunny chunk");

			for (const name of ["element", "ui/element", "index"]) {
				const entry = [...chunks.values()].find((chunk) => chunk.isEntry && chunk.name === name);
				if (!entry) throw new Error(`Missing ${name} entrypoint`);
				const eager = new Set<string>();
				const visit = (file: string) => {
					if (eager.has(file)) return;
					const chunk = chunks.get(file);
					if (!chunk) return; // Workspace dependencies remain external.
					eager.add(file);
					for (const imported of chunk.imports) visit(imported);
				};
				visit(entry.fileName);
				for (const chunk of media) assert(!eager.has(chunk.fileName), `${name} eagerly imports mediabunny`);
				// The UI only attaches controls; the publisher and public source entrypoints own decoding.
				if (name !== "ui/element") {
					assert(
						[...eager].some((file) =>
							chunks
								.get(file)
								?.dynamicImports.some((imported) => media.some((chunk) => chunk.fileName === imported)),
						),
						`${name} must dynamically import mediabunny`,
					);
				}
			}
		},
	};
}
