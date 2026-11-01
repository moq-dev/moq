import * as Moq from "@moq/net";

const FOCUS_PARAM = "truck";

/** Preview, focus, and drive state shared by fleet navigation and truck sessions. */
export class Interaction {
	readonly focused: Moq.Signals.Signal<string | undefined>;
	readonly hovered = new Moq.Signals.Signal<string | undefined>(undefined);
	readonly driving = new Moq.Signals.Signal<string | undefined>(undefined);

	constructor(focused?: string) {
		this.focused = new Moq.Signals.Signal(focused);
	}

	/** Focus a truck for viewing. Leaving or changing focus releases control. */
	focus(id: string | undefined) {
		const active = this.driving.peek();
		if (active && active !== id) this.driving.set(undefined);
		if (this.focused.peek() !== id) this.hovered.set(undefined);
		this.focused.set(id);
	}

	/** Take control of the focused truck, including from a drive-intent key. */
	takeDrive(id: string): boolean {
		if (this.focused.peek() !== id) return false;
		this.driving.set(id);
		return true;
	}

	/** Release control without leaving the focused view. */
	releaseDrive(): boolean {
		if (!this.driving.peek()) return false;
		this.driving.set(undefined);
		return true;
	}

	/** Take or release control of the focused truck from the explicit button. */
	toggleDrive(id: string) {
		if (this.driving.peek() === id) {
			this.releaseDrive();
		} else {
			this.takeDrive(id);
		}
	}

	/** Drop every local state tied to a truck that disappeared. */
	remove(id: string): boolean {
		if (this.hovered.peek() === id) this.hovered.set(undefined);
		if (this.driving.peek() === id) this.driving.set(undefined);
		if (this.focused.peek() !== id) return false;
		this.focused.set(undefined);
		return true;
	}

	/** Truck whose e-stop must remain reachable through a covering modal. */
	safetyTarget(): string | undefined {
		return this.focused.peek() ?? this.driving.peek();
	}
}

/** Read the focused truck from the URL fragment. */
export function focusFromHash(hash: string): string | undefined {
	const value = new URLSearchParams(hash.replace(/^#/, "")).get(FOCUS_PARAM);
	return value || undefined;
}

/** Return a fragment with the focused truck changed and other state preserved. */
export function hashWithFocus(hash: string, truck: string | undefined): string {
	const params = new URLSearchParams(hash.replace(/^#/, ""));
	if (truck) {
		params.set(FOCUS_PARAM, truck);
	} else {
		params.delete(FOCUS_PARAM);
	}
	const value = params.toString();
	return value ? `#${value}` : "";
}

/**
 * Whether a key aimed at `target` belongs to the player rendered in `root`.
 *
 * The window sees every key on the host page, so the player takes only keys aimed
 * inside it or at nothing in particular; the host page's own controls keep theirs.
 */
export function ownsKey(target: EventTarget | undefined, root: ShadowRoot): boolean {
	if (!target || !("nodeType" in target)) return true;
	const node = target as Node;
	const doc = root.ownerDocument;
	return node === doc.body || node === doc.documentElement || node === root.host || root.contains(node);
}
