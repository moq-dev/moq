/**
 * React wrapper generator
 *
 * Generates React components that wrap custom elements, providing:
 * - Proper TypeScript types
 * - React.forwardRef for ref forwarding
 * - JSX intrinsic element declarations
 * - Complete JSDoc documentation from CEM
 */

import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { formatCode, generateJSDoc } from "../utils/codegen";
import { extractCustomElements, loadManifest, tagNameToComponentName } from "../utils/manifest";
import type { CustomElement } from "../utils/types";

/**
 * Generate TypeScript interface for custom element attributes
 *
 * Creates an interface with properly typed attributes from the custom element.
 */
function generateAttributesInterface(element: CustomElement): string {
	if (!element.attributes || element.attributes.length === 0) {
		return "React.HTMLAttributes<HTMLElement>";
	}

	const attrs = element.attributes
		.map((attr) => {
			let type = "string";
			if (attr.type?.text) {
				// Map CEM types to TypeScript types
				if (attr.type.text === "boolean") {
					type = "boolean";
				} else if (attr.type.text === "number") {
					type = "number";
				} else {
					type = "string";
				}
			}
			// Attributes are optional in JSX
			return `    "${attr.name}"?: ${type};`;
		})
		.join("\n");

	return `React.HTMLAttributes<HTMLElement> & {\n${attrs}\n  }`;
}

/**
 * Generate a React wrapper component for a custom element
 *
 * Creates a forwardRef component that renders the custom element
 * with proper TypeScript types and JSDoc documentation.
 */
function generateReactComponent(element: CustomElement): string {
	const componentName = tagNameToComponentName(element.tagName);
	const jsDoc = generateJSDoc(
		element.summary,
		element.description,
		element.slots,
		element.events,
		element.attributes,
		element.properties,
		element.examples,
	);

	const attributesType = generateAttributesInterface(element);

	return `${jsDoc}
export const ${componentName} = React.forwardRef<
  HTMLElement,
  ${attributesType} & {
    children?: React.ReactNode;
  }
>(({ children, ...props }, ref) => {
  return React.createElement("${element.tagName}", { ...props, ref }, children);
});

${componentName}.displayName = "${componentName}";
`;
}

/**
 * Generate TypeScript module augmentation for JSX intrinsic elements
 *
 * Declares custom elements in React.JSX.IntrinsicElements so TypeScript
 * recognizes them as valid JSX elements.
 */
function generateJSXModuleAugmentation(elements: CustomElement[]): string {
	if (elements.length === 0) return "";

	const intrinsicElements = elements
		.map((el) => {
			const attrType = generateAttributesInterface(el);
			return `\t\t\t"${el.tagName}": ${attrType} & { children?: React.ReactNode };`;
		})
		.join("\n");

	return `
declare module "react" {
\tnamespace JSX {
\t\tinterface IntrinsicElements {
${intrinsicElements}
\t\t}
\t}
}
`;
}

/**
 * Generate file header with auto-generation warning and timestamp
 */
function generateFileHeader(): string {
	const timestamp = new Date().toISOString();
	return `/**
 * Auto-generated React wrappers for custom elements
 * DO NOT EDIT MANUALLY - Generated from custom-elements.json
 *
 * Generated: ${timestamp}
 */

import React from "react";
`;
}

/**
 * Generate React wrappers for all custom elements
 *
 * Main entry point for React wrapper generation. Loads CEM, extracts elements,
 * generates wrapper components, and writes to src/wrappers/react/index.ts.
 *
 * @param basePath - Project root directory
 */
export function generateReactWrappers(basePath: string = process.cwd()): void {
	console.log("\n🔧 Generating React wrappers...");

	try {
		const manifest = loadManifest(basePath);
		const elements = extractCustomElements(manifest);

		if (elements.length === 0) {
			console.warn("⚠️  No custom elements found");
			return;
		}

		const header = generateFileHeader();
		const components = elements.map(generateReactComponent).join("\n");
		const moduleAugmentation = generateJSXModuleAugmentation(elements);

		const output = `${header}\n${components}\n${moduleAugmentation}`;

		const outputDir = join(basePath, "src", "wrappers", "react");
		const outputPath = join(outputDir, "index.ts");

		mkdirSync(outputDir, { recursive: true });
		writeFileSync(outputPath, formatCode(output));

		console.log(
			`✅ Generated ${elements.length} wrapper${elements.length > 1 ? "s" : ""}: src/wrappers/react/index.ts`,
		);
		for (const el of elements) {
			console.log(`   └─ ${tagNameToComponentName(el.tagName)} ← <${el.tagName}>`);
		}
	} catch (error) {
		const errorMsg = error instanceof Error ? error.message : String(error);
		console.error(`❌ Failed to generate React wrappers: ${errorMsg}`);
		throw error;
	}
}
