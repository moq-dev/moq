// Packages mode converts each package on its own, so a package that re-exports another
// (`export * as Net from "@moq/net"`) carries a full copy of it. After the merge, swap each copy
// for a reference to the owning package's reflection, so it renders as a link, not a duplicate.
import {
	Application,
	type CommentDisplayPart,
	type DeclarationReflection,
	ReferenceReflection,
	Reflection,
	type ReflectionGroup,
	ReflectionKind,
} from "typedoc";

/** Link re-exports to the package that owns them; `dirs` maps a package name to its directory. */
export function dedupe(app: Application, dirs: Map<string, string>) {
	app.on(Application.EVENT_PROJECT_REVIVE, (project) => {
		// The top-level module is the package, and its directory owns the files under it.
		const packageDir = (reflection: Reflection) => {
			let top = reflection;
			while (top.parent && top.parent !== project) top = top.parent;
			return dirs.get(top.name);
		};
		const file = (reflection: Reflection) =>
			reflection.isDeclaration() ? reflection.sources?.[0]?.fileName : undefined;
		const sourceDir = (reflection: Reflection) => {
			const name = file(reflection);
			return name && [...dirs.values()].find((dir) => name.startsWith(`${dir}/`));
		};
		// A declaration's position identifies it across copies, even under another export name.
		const position = (reflection: Reflection) => {
			const source = reflection.isDeclaration() ? reflection.sources?.[0] : undefined;
			return source && `${source.fileName}:${source.line}:${source.character}:${reflection.kind}`;
		};

		const owned = new Map<string, Reflection>();
		const modules = new Map<string, Reflection>();
		const copies: DeclarationReflection[] = [];
		for (const reflection of Object.values(project.reflections)) {
			const dir = sourceDir(reflection);
			if (!dir || !reflection.isDeclaration()) continue;
			if (dir === packageDir(reflection)) {
				const key = position(reflection);
				if (key && !owned.has(key)) owned.set(key, reflection);
				// `export * as Net from "@moq/net"` names the dependency's entry file.
				if (reflection.kindOf(ReflectionKind.Module))
					for (const source of reflection.sources ?? []) modules.set(source.fileName, reflection);
				continue;
			}
			// Only the exported name is a copy; its members go with it.
			if (reflection.kindOf(ReflectionKind.Module)) continue;
			if (!reflection.parent?.kindOf(ReflectionKind.SomeModule)) continue;
			if (sourceDir(reflection.parent) === dir) continue;
			copies.push(reflection);
		}

		// Removed copies, and everything in them, by the owner's declaration that replaces them.
		const aliases = new Map<Reflection, Reflection>();
		let unmatched = 0;
		for (const copy of copies) {
			// Already gone with an enclosing copy.
			if (aliases.has(copy)) continue;
			const name = file(copy);
			const target =
				owned.get(position(copy) ?? "") ??
				(copy.kindOf(ReflectionKind.Namespace) && name ? modules.get(name) : undefined);
			if (!target) {
				// The owner does not export it, so this copy is the only place it is documented.
				unmatched++;
				app.logger.verbose(`dedupe: ${copy.getFullName()} has no exported owner`);
				continue;
			}
			aliases.set(copy, target);
			copy.traverse(function visit(child) {
				aliases.set(child, owned.get(position(child) ?? "") ?? target);
				child.traverse(visit);
				return true;
			});
			const parent = copy.parent as DeclarationReflection;
			project.removeReflection(copy);
			const reference = new ReferenceReflection(copy.name, target, parent);
			project.registerReflection(reference, undefined, undefined);
			parent.addChild(reference);
			// Groups were built before the merge, and removal leaves them pointing at the copy.
			const swap = (children: ReflectionGroup["children"]) =>
				children.map((child) => (child === copy ? reference : child));
			for (const group of parent.groups ?? []) {
				group.children = swap(group.children);
				for (const category of group.categories ?? []) category.children = swap(category.children);
			}
			for (const category of parent.categories ?? []) category.children = swap(category.children);
		}

		// TypeDoc resolves a type's target by id when rendering, and has no API to retarget one.
		for (const [copy, owner] of aliases) project.reflections[copy.id] = owner;
		// A comment's link holds its target directly.
		const retarget = (parts: CommentDisplayPart[]) => {
			for (const part of parts) {
				if (part.kind !== "inline-tag" || !(part.target instanceof Reflection)) continue;
				part.target = aliases.get(part.target) ?? part.target;
			}
		};
		for (const reflection of Object.values(project.reflections)) {
			if (!reflection.comment) continue;
			retarget(reflection.comment.summary);
			for (const tag of reflection.comment.blockTags) retarget(tag.content);
		}
		app.logger.verbose(`dedupe: ${copies.length - unmatched} re-exports linked, ${unmatched} kept`);
	});
}
