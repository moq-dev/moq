// One page per package entrypoint, such as `net.md` for `@moq/net` and `net/zod.md` for
// `@moq/net/zod`. Every symbol on it, namespace members included, gets an anchor.
import { type PageDefinition, PageKind, type Reflection, ReflectionKind, Slugger } from "typedoc";
import { ModuleRouter } from "typedoc-plugin-markdown";

// typedoc-vitepress-theme re-slugs the anchor of every link it rewrites, so an anchor must
// already be in its form or the link and the heading's `{#anchor}` disagree.
class Anchors extends Slugger {
	override slug(value: string) {
		const slug = value
			.toLowerCase()
			.replace(/[^a-z0-9]+/g, "-")
			.replace(/^-+|-+$/g, "")
			.replace(/^(\d)/, "_$1");
		return super.slug(slug || "_");
	}
}

/** Routes each entrypoint module to its own page, named by its import specifier without the scope. */
export class EntrypointRouter extends ModuleRouter {
	override buildChildPages(reflection: Reflection, pages: PageDefinition[]) {
		if (!reflection.kindOf(ReflectionKind.Module))
			throw new Error(`unexpected top-level ${reflection.getFullName()}`);
		const entries = reflection.isDeclaration()
			? (reflection.children ?? []).filter((child) => child.kindOf(ReflectionKind.Module))
			: [];
		if (entries.length === 0) return this.page(reflection, pages);
		for (const entry of entries) this.page(entry, pages);
		// The package itself links to its main entrypoint.
		const main = entries.find((entry) => entry.name === reflection.name);
		if (!main) throw new Error(`${reflection.name} has no "." entrypoint`);
		this.fullUrls.set(reflection, this.getFullUrl(main));
	}

	private page(reflection: Reflection, pages: PageDefinition[]) {
		const url = this.getFileName(reflection.name.replace(/^@[^/]+\//, ""));
		this.fullUrls.set(reflection, url);
		this.sluggers.set(reflection, new Anchors(this.sluggerConfiguration));
		pages.push({ kind: PageKind.Reflection, model: reflection, url });
		reflection.traverse((child) => {
			this.buildAnchors(child, reflection);
			return true;
		});
	}
}
