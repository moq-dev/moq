import type { Cost } from "../hop.ts";
import type * as Instance from "../instance.ts";
import { coveringPrefixes, hiddenBelow } from "../internal.ts";
import * as Path from "../path.ts";

/** A claim as an announce stream carries it: the instance, its epoch, and the origin's cost. @internal */
export type Claim = { instance: Instance.Valid; epoch: bigint; cost: Cost };

/**
 * One announce stream's claims for a route, as its last floor left them. `revision` is the
 * origin's revision that floor certified them at, or undefined when it certified nothing.
 * @internal
 */
export interface View {
	readonly prefix: Path.Valid;
	// Whether the request asked for hidden claims: one that did not certifies nothing about them.
	readonly hidden: boolean;
	readonly revision: bigint | undefined;
	readonly claims: ReadonlyMap<Path.Valid, Claim>;
}

/** A route on one announce stream, whose claims another stream's floor may hide. @internal */
export interface Carrier {
	readonly view: View;
	/** Show or hide each of its claims anew, once another stream's floor moved what is current. */
	reshow(): void;
}

type Node = { carriers: Set<Carrier>; retained: View[] };

// Whether `view` certifies whether a claim at `path` exists.
function certifies(view: View, path: Path.Valid): boolean {
	return (
		view.revision !== undefined &&
		Path.hasPrefix(view.prefix, path) &&
		(view.hidden || !hiddenBelow(view.prefix, path))
	);
}

// Whether `view` certifies every path `other` does, at a revision at least as new.
function supersedes(view: View, other: View): boolean {
	return (
		view.revision !== undefined &&
		other.revision !== undefined &&
		view.revision >= other.revision &&
		Path.hasPrefix(view.prefix, other.prefix) &&
		(view.hidden || (!other.hidden && !hiddenBelow(view.prefix, other.prefix)))
	);
}

// Whether two prefixes overlap: one lies within the other.
function meets(a: Path.Valid, b: Path.Valid): boolean {
	return Path.hasPrefix(a, b) || Path.hasPrefix(b, a);
}

// The claim `view` presents at `path`: exactly there, or with `covering`, at the requesting
// stream's own prefix, the most specific claim at or above it within the view.
function presented(view: View, path: Path.Valid, covering: boolean): Claim | undefined {
	if (!covering) return view.claims.get(path);
	for (const at of coveringPrefixes(path)) {
		if (!Path.hasPrefix(view.prefix, at)) return undefined;
		const found = view.claims.get(at);
		if (found) return found;
	}
	return undefined;
}

/**
 * What a session's announce streams certify of each origin node's claims (lite-07).
 *
 * Several streams may carry one origin's claims, each answering its own prefix and applying its
 * batches at its own floors, so one can lag another. Of the views whose floor certifies a claim,
 * the one with the highest revision decides whether it exists, and a claim no floor certifies
 * exists while some stream carries it. A route that ends, or whose floor stops certifying or goes
 * back, leaves what its last floor certified, so an older floor never revives a claim a newer one
 * withdrew; that knowledge counts while a live floor still certifies the path. Mirrors `route/view.rs`'s merge in
 * rs/moq-net, held per session since a node that forwards nothing merges only for its own reads.
 * @internal
 */
export class Floors {
	#nodes = new Map<bigint, Node>();

	/** Start tracking a route to `node`. */
	join(node: bigint, carrier: Carrier): void {
		let entry = this.#nodes.get(node);
		if (!entry) {
			entry = { carriers: new Set(), retained: [] };
			this.#nodes.set(node, entry);
		}
		entry.carriers.add(carrier);
	}

	/**
	 * The route ended or its stream closed. What its floor certified stays while another route to
	 * the node may still carry a claim it decides; the last route takes the node's state with it.
	 * Every other route to the node whose prefix meets this one's is shown anew, since a path this
	 * one alone certified now shows what its streams carry; a route's claims lie within its prefix,
	 * so no other route's can change, and a session closing many prefixes costs each only its own.
	 */
	leave(node: bigint, carrier: Carrier): void {
		const entry = this.#nodes.get(node);
		if (!entry?.carriers.delete(carrier)) return;
		if (entry.carriers.size === 0) {
			this.#nodes.delete(node);
			return;
		}
		// The route's own state is gone with it, so its claims need no copy.
		const { prefix, hidden, revision, claims } = carrier.view;
		if (revision !== undefined) entry.retained.push({ prefix, hidden, revision, claims });
		this.#prune(entry);
		for (const other of entry.carriers) if (meets(other.view.prefix, prefix)) other.reshow();
	}

	/**
	 * A route to `node` is about to apply a floor that certifies nothing, or an older revision.
	 * What its current floor certified stays, as an ended route's does, since no floor withdraws
	 * what a newer one said. Call it before the batch changes the route's view.
	 */
	regress(node: bigint, carrier: Carrier): void {
		const entry = this.#nodes.get(node);
		const { prefix, hidden, revision, claims } = carrier.view;
		if (!entry?.carriers.has(carrier) || revision === undefined) return;
		entry.retained.push({ prefix, hidden, revision, claims: new Map(claims) });
	}

	/**
	 * A route to `node` applied a batch at its floor: every other route to it whose prefix meets
	 * this one's is shown anew, as in `leave`. Returns whether any other view of the node existed,
	 * in which case the route's own unchanged claims may have changed state too, even if this floor
	 * pruned it.
	 */
	floor(node: bigint, carrier: Carrier): boolean {
		const entry = this.#nodes.get(node);
		if (!entry) return false;
		const retained = entry.retained.length;
		this.#prune(entry);
		if (entry.carriers.size === 1 && retained === 0) return false;
		const prefix = carrier.view.prefix;
		for (const other of entry.carriers) if (other !== carrier && meets(other.view.prefix, prefix)) other.reshow();
		return true;
	}

	/**
	 * The claim `view` shows at `path`, under a route to `node`: its own when no view certifies the
	 * path, or else the newest certifying view's, so its cost is current, when that one presents the
	 * same instance and epoch there. Undefined when the claim does not exist.
	 */
	shows(node: bigint, view: View, path: Path.Valid): Claim | undefined {
		const claim = view.claims.get(path);
		if (!claim) return undefined;
		const entry = this.#nodes.get(node);
		// The view itself wins a tie: one revision presents the same claims on every stream.
		let best: View | undefined = certifies(view, path) ? view : undefined;
		const consider = (other: View) => {
			if (!certifies(other, path)) return;
			// biome-ignore lint/style/noNonNullAssertion: certifies() checked both revisions.
			if (best === undefined || other.revision! > best.revision!) best = other;
		};
		for (const other of entry?.carriers ?? []) consider(other.view);
		// What an ended or regressed floor said counts only where a live one still certifies, as
		// Rust's router prunes a merged region no certifier certifies any more.
		if (best !== undefined) for (const other of entry?.retained ?? []) consider(other);
		if (best === undefined || best === view) return claim;
		const theirs = presented(best, path, path === view.prefix);
		if (theirs?.instance !== claim.instance || theirs.epoch !== claim.epoch) return undefined;
		return theirs;
	}

	/**
	 * Whether `view` or a live view of `node` certifies whether a claim at `path` exists, so a
	 * claim gone there was withdrawn by its origin rather than let go, as rs/moq-net's router
	 * tells an end from going offline.
	 */
	certified(node: bigint, view: View, path: Path.Valid): boolean {
		if (certifies(view, path)) return true;
		for (const carrier of this.#nodes.get(node)?.carriers ?? []) if (certifies(carrier.view, path)) return true;
		return false;
	}

	/**
	 * Whether a claim of `node` at `at` may serve a consume of `path` below it. The newest view
	 * certifying `path`, the most specific of those alike, certifies what covers it there: a claim
	 * above that view's region is older knowledge, as `covering` in rs/moq-net's `route/view.rs`.
	 */
	covers(node: bigint, at: Path.Valid, path: Path.Valid): boolean {
		const entry = this.#nodes.get(node);
		if (!entry || at === path) return true;
		let best: View | undefined;
		const consider = (view: View) => {
			if (!certifies(view, path)) return;
			if (
				best === undefined ||
				// biome-ignore lint/style/noNonNullAssertion: certifies() checked both revisions.
				view.revision! > best.revision! ||
				(view.revision === best.revision && view.prefix.length > best.prefix.length)
			)
				best = view;
		};
		for (const carrier of entry.carriers) consider(carrier.view);
		// As in shows(): a retained view counts only where a live floor still certifies.
		if (best !== undefined) for (const view of entry.retained) consider(view);
		return best === undefined || Path.hasPrefix(best.prefix, at);
	}

	// Drop each retained view that decides no claim a route holds: one another view supersedes, or
	// one no certifying route's prefix meets. Of two alike, the later stays, so each prefix the session
	// requested keeps at most two: one for the hidden claims, one for the rest.
	#prune(entry: Node): void {
		const live = [...entry.carriers].map((carrier) => carrier.view);
		const retained = entry.retained;
		entry.retained = retained.filter(
			(view, i) =>
				live.some((other) => other.revision !== undefined && meets(other.prefix, view.prefix)) &&
				!live.some((other) => supersedes(other, view)) &&
				!retained.some((other, j) => j !== i && supersedes(other, view) && (j > i || !supersedes(view, other))),
		);
	}
}
