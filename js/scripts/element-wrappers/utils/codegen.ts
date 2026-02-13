/**
 * Code generation utilities
 *
 * Provides functions for:
 * - Formatting generated code
 * - Generating JSDoc comments from metadata
 * - Escaping special characters for documentation
 */

import type { Attribute, EventDeclaration, Property, Slot } from "./types";

/**
 * Format generated code with consistent trailing newline
 */
export function formatCode(code: string): string {
	return `${code.trim()}\n`;
}

/**
 * Escape special characters in JSDoc content
 * Prevents premature closing of JSDoc blocks
 */
function escapeJSDoc(text: string): string {
	return text.replace(/\*\//g, "*\\/");
}

/**
 * Format a documentation section with consistent indentation
 *
 * @param title - Section title (e.g., "slots", "events")
 * @param items - Array of items with name, description, and optional extra text
 * @returns Array of formatted JSDoc lines
 */
function formatDocSection(
	title: string,
	items: Array<{ name: string; description?: string; extra?: string }>,
): string[] {
	if (items.length === 0) return [];

	return [
		` * @${title}`,
		...items.map((item) => {
			const extra = item.extra ?? "";
			const desc = item.description ? ` - ${escapeJSDoc(item.description)}` : "";
			return ` * - \`${item.name}\`${extra}${desc}`;
		}),
	];
}

/**
 * Generate JSDoc comment block from custom element metadata
 *
 * Creates comprehensive JSDoc with @summary, @description, section tags
 * (@slots, @events, @attributes, @properties), and multiple @example blocks.
 *
 * @param summary - Brief one-line summary
 * @param description - Detailed description
 * @param slots - Slot definitions
 * @param events - Event definitions
 * @param attributes - Attribute definitions
 * @param properties - Property definitions
 * @param examples - Labeled code examples (e.g., {"HTML": "...", "React": "..."})
 * @returns Formatted JSDoc comment block
 */
export function generateJSDoc(
	summary?: string,
	description?: string,
	slots?: Slot[],
	events?: EventDeclaration[],
	attributes?: Attribute[],
	properties?: Property[],
	examples?: Record<string, string>,
): string {
	const lines: string[] = ["/**"];

	// Add summary
	if (summary) {
		lines.push(` * @summary ${escapeJSDoc(summary)}`);
	}

	// Add description only if different from summary
	if (description && description !== summary) {
		lines.push(` * @description ${escapeJSDoc(description)}`);
	}

	// Add documentation sections
	if (slots?.length) {
		lines.push(
			...formatDocSection(
				"slots",
				slots.map((slot) => ({
					name: slot.name || "default",
					description: slot.description,
				})),
			),
		);
	}

	if (events?.length) {
		lines.push(
			...formatDocSection(
				"events",
				events.map((event) => ({
					name: event.name,
					description: event.description,
				})),
			),
		);
	}

	if (attributes?.length) {
		lines.push(
			...formatDocSection(
				"attributes",
				attributes.map((attr) => ({
					name: attr.name,
					description: attr.description,
				})),
			),
		);
	}

	if (properties?.length) {
		lines.push(
			...formatDocSection(
				"properties",
				properties.map((prop) => ({
					name: prop.name,
					description: prop.description,
					extra: prop.readonly ? " (readonly)" : "",
				})),
			),
		);
	}

	// Add examples with proper JSDoc formatting
	if (examples && Object.keys(examples).length > 0) {
		const exampleEntries = Object.entries(examples);
		for (let i = 0; i < exampleEntries.length; i++) {
			const [label, content] = exampleEntries[i];
			// Use @example with caption for label
			lines.push(` * @example`);
			lines.push(` * <caption>${label}</caption>`);
			// Remove markdown code fences and add proper indentation
			const exampleLines = content.split("\n");
			for (const line of exampleLines) {
				// Skip markdown code fence lines (```html, ```tsx, ```)
				if (line.trim().startsWith("```")) {
					continue;
				}
				// Add indentation for code (standard JSDoc format)
				if (line.trim()) {
					lines.push(` * ${line}`);
				} else {
					lines.push(` *`);
				}
			}
			// Add empty line between examples for better separation
			if (i < exampleEntries.length - 1) {
				lines.push(` *`);
			}
		}
	}

	lines.push(` */`);
	return lines.join("\n");
}
