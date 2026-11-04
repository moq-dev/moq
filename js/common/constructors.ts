// stripInternal must not turn an explicit constructor into an implicit public one.
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { parse } from "@babel/parser";

function classes(source: string, jsx: boolean) {
	const ast = parse(source, {
		sourceType: "module",
		plugins: jsx ? ["typescript", "jsx"] : ["typescript"],
	});
	type Statement = (typeof ast.program.body)[number];
	const found = new Map<string, Extract<Statement, { type: "ClassDeclaration" }>>();
	function visit(statement: Statement, scope = "") {
		const declaration =
			statement.type === "ExportNamedDeclaration" || statement.type === "ExportDefaultDeclaration"
				? statement.declaration
				: statement;
		if (declaration?.type === "ClassDeclaration") {
			found.set(scope + (declaration.id?.name ?? "default"), declaration);
		} else if (declaration?.type === "TSModuleDeclaration") {
			const name = source.slice(declaration.id.start ?? 0, declaration.id.end ?? source.length);
			const prefix = `${scope}${name}.`;
			if (declaration.body?.type === "TSModuleBlock") {
				for (const child of declaration.body.body) visit(child, prefix);
			}
		}
	}
	for (const statement of ast.program.body) visit(statement);
	return found;
}

/** Check that emitted classes retain their source constructors' visibility and arity. */
export function checkConstructors(source: string, declaration: string, jsx = false): void {
	const originals = classes(source, jsx);
	for (const [name, emitted] of classes(declaration, false)) {
		const original = originals.get(name);
		if (!original) throw new Error(`No source class for ${name}`);
		type Constructor = Extract<(typeof original.body.body)[number], { type: "ClassMethod" | "TSDeclareMethod" }>;
		const constructors = (body: typeof original.body.body) =>
			body.filter(
				(member): member is Constructor =>
					(member.type === "ClassMethod" || member.type === "TSDeclareMethod") &&
					member.kind === "constructor",
			);
		const sourceConstructors = constructors(original.body.body);
		// An overload's implementation is not part of its public signature.
		const overloads = sourceConstructors.filter((member) => member.type === "TSDeclareMethod");
		const signatures = (members: typeof sourceConstructors) =>
			members.map((member) =>
				JSON.stringify([
					member.accessibility ?? "public",
					member.accessibility === "private" ? 0 : member.params.length,
				]),
			);
		if (
			JSON.stringify(signatures(overloads.length ? overloads : sourceConstructors)) !==
			JSON.stringify(signatures(constructors(emitted.body.body)))
		) {
			throw new Error(
				`${name}: declaration changed constructor visibility or arity; use a private constructor and a module factory`,
			);
		}
	}
}

/** Verify every built class, including classes in private packages, before packaging. */
export function checkPackageConstructors(directory: string): void {
	for (const file of new Bun.Glob("**/*.d.ts").scanSync(join(directory, "dist"))) {
		let source = join(directory, "src", file.replace(/\.d\.ts$/, ".ts"));
		if (!existsSync(source)) source += "x";
		if (!existsSync(source)) continue;
		try {
			checkConstructors(
				readFileSync(source, "utf8"),
				readFileSync(join(directory, "dist", file), "utf8"),
				source.endsWith(".tsx"),
			);
		} catch (error) {
			throw new Error(`${source}: ${error instanceof Error ? error.message : error}`);
		}
	}
}
