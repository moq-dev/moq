// typedoc-plugin-markdown's theme, with namespaces rendered in full on their entrypoint's page
// instead of as a bare title, since the router gives them no page of their own.
import { type DeclarationReflection, type PageEvent, type Reflection, ReflectionKind } from "typedoc";
import { type MarkdownPageEvent, MarkdownTheme, MarkdownThemeContext } from "typedoc-plugin-markdown";

class Context extends MarkdownThemeContext {
	constructor(...args: ConstructorParameters<typeof MarkdownThemeContext>) {
		super(...args);
		const member = this.partials.member;
		this.partials.member = (model: DeclarationReflection, options) =>
			model.kindOf(ReflectionKind.Namespace)
				? this.partials.memberWithGroups(model, { headingLevel: options.headingLevel + 1 })
				: member(model, options);
		// A constructor's heading is this partial's rather than its title's, so `useCustomAnchors`
		// drops the anchor every `Overrides` line links to.
		const ctor = this.partials.constructor;
		this.partials.constructor = (model: DeclarationReflection, options) =>
			ctor(model, options).replace(/^(#+ Constructor)$/m, `$1 {#${this.router.getAnchor(model)}}`);
	}
}

/** The markdown theme, with inline namespaces. */
export class Theme extends MarkdownTheme {
	override getRenderContext(page: MarkdownPageEvent<Reflection>) {
		return new Context(this, page, this.application.options);
	}
}

/** Finish a page for VitePress. */
export function finish(page: PageEvent) {
	if (page.contents === undefined) return;
	// Generated, so there is no page to edit and no history to date it by.
	const frontmatter = "---\neditLink: false\nlastUpdated: false\n---\n\n";
	// Nested namespaces go deeper than h6, the deepest heading markdown has.
	page.contents = frontmatter + page.contents.replace(/^#{7,} /gm, "###### ");
}
