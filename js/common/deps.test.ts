import { describe, expect, test } from "bun:test";
import { specifiers } from "./deps";

describe("specifiers", () => {
	test("finds module references", () => {
		const source = `
			import "@moq/side-effect";
			import { Value } from "@moq/static";
			import type { Type } from "@moq/type";
			import Equals = require("@moq/equals");
			type Query = import("@moq/query").Query;
			export { Other } from "@moq/export";
			const dynamic = import("@moq/dynamic");
			const template = import(\`@moq/template\`);
			const required = require("@moq/require");
		`;

		expect(specifiers(source)).toEqual([
			"@moq/side-effect",
			"@moq/static",
			"@moq/type",
			"@moq/equals",
			"@moq/query",
			"@moq/export",
			"@moq/dynamic",
			"@moq/template",
			"@moq/require",
		]);
	});

	test("reads declaration files", () => {
		const source = `
			import { type Report, Support } from "@moq/static";
			export { type Show } from "@moq/export";
			export default class Banner extends Support {
				protected detect(): Promise<Report>;
			}
			export declare function rows(): import("@moq/query").Rows;
		`;

		expect(specifiers(source)).toEqual(["@moq/static", "@moq/export", "@moq/query"]);
	});

	test("ignores comments and strings", () => {
		const source = `
			// import "@moq/comment";
			/* export { Value } from "@moq/block-comment"; */
			const quoted = 'import("@moq/quoted")';
			const template = \`require("@moq/template")\`;
		`;

		expect(specifiers(source)).toEqual([]);
	});
});
