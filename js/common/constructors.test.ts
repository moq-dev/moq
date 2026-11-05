import { expect, test } from "bun:test";
import { checkConstructors } from "./constructors.ts";

test("declaration emit must preserve explicit construction, including overloads and defaults", () => {
	expect(() =>
		checkConstructors(
			"export class Handle { /** @internal */ constructor(state: State) {} }",
			"export declare class Handle {}",
		),
	).toThrow("Handle: declaration changed constructor");
	expect(() =>
		checkConstructors(
			"class Handle { private constructor(state: State) {} }",
			"declare class Handle { constructor(); }",
		),
	).toThrow("Handle: declaration changed constructor");
	expect(() =>
		checkConstructors("class Handle { constructor(state: State) {} }", "declare class Handle { constructor(); }"),
	).toThrow("Handle: declaration changed constructor");
	checkConstructors(
		"export default class Handle { private constructor(state: State) {} }",
		"export default class Handle { private constructor(); }",
	);
	checkConstructors(
		"class Handle { constructor(state: State); constructor(state = defaultState) {} }",
		"declare class Handle { constructor(state: State); }",
	);
	checkConstructors(
		"class Handle { constructor(state = defaultState) {} }",
		"declare class Handle { constructor(state?: State); }",
	);
	checkConstructors("class Implicit {}", "declare class Implicit {}");
	checkConstructors("/** @internal */ class Hidden { constructor(state: State) {} }", "");
	expect(() =>
		checkConstructors(
			"export namespace Nested { export class Handle { /** @internal */ constructor() {} } }",
			"export namespace Nested { export class Handle {} }",
		),
	).toThrow("Nested.Handle: declaration changed constructor");
	expect(() =>
		checkConstructors("export default class { constructor(state: State) {} }", "export default class {}"),
	).toThrow("default: declaration changed constructor");
});
