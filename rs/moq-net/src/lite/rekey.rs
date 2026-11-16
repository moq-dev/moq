//! Lite-07: how an Announce Stream's live routes carry over an edit of its requested
//! prefixes, at the ANNOUNCE_OK answering it.
//!
//! Both endpoints run [`plan`] over what the stream carries, so they agree on which
//! routes and claims live on, and under which ids, without a message for any of it.

use std::{
	cmp::Reverse,
	collections::{BTreeMap, BTreeSet},
};

use crate::{
	Path,
	route::{self, Metric, ancestors, path::Offer, within},
};

/// How a route ranks among the routes that would share an origin node and prefix: one
/// that is not held first, then one whose path does not name the subscriber, then the
/// better rank.
pub(crate) type Order = (bool, bool, (bool, Reverse<i8>, Metric));

/// The [`Order`] of a route offering `offer`, or the path of an offer the receiver
/// refused, to the subscriber `receiver`.
pub(crate) fn order(held: bool, offer: Result<&Offer, &[route::path::Hop]>, receiver: route::Id) -> Order {
	let names = |path: &[route::path::Hop]| path.iter().any(|hop| hop.node == receiver);
	match offer {
		Ok(offer) => (held, names(&offer.path), offer.rank()),
		Err(path) => (held, names(path), (true, Reverse(i8::MIN), Metric::MAX)),
	}
}

/// A live route on the stream.
pub(crate) struct Route {
	pub node: route::Id,
	/// The prefix it covers, or the requested prefix it lies in when its suffix was
	/// refused.
	pub prefix: Path,
	/// The requested prefix it lies in.
	pub region: Path,
	/// Its ROUTE_START said its sender keeps it at its own prefix, so a widening leaves
	/// it there rather than moving it up.
	pub exact: bool,
	pub order: Order,
	/// The Announce ID of each of its live claims with a path.
	pub claims: BTreeMap<Path, u64>,
}

/// What became of a route that lives on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
	/// Unchanged.
	Stays,
	/// Moved to a prefix above the one it was requested under: it certifies nothing
	/// until its next floor.
	Widens,
	/// Carried to a prefix below the one it covered, with its floor.
	Narrows,
}

/// A claim of a route that lives on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Claim {
	/// Its Announce ID from here on.
	pub id: u64,
	/// The Announce ID it had, the same unless it is now presented at a prefix below
	/// its path.
	pub from: u64,
}

/// A route that lives on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Carried {
	/// Its Route ID from here on.
	pub id: u64,
	/// The route whose offer and state it keeps.
	pub from: u64,
	pub kind: Kind,
	pub prefix: Path,
	pub region: Path,
	/// It lies at its own prefix: one that stays keeps it, and one that widens or
	/// narrows is presented at a prefix that is not its own.
	pub exact: bool,
	pub claims: BTreeMap<Path, Claim>,
}

/// The routes that live on, by their new Route ID. Every other route ends, and every
/// Announce ID no claim here keeps retires.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Plan {
	pub routes: Vec<Carried>,
	pub next_route: u64,
	pub next_claim: u64,
}

/// Carry `routes` over an edit from the prefixes `old` to `new`, which added `added` in
/// that order. `next_route` and `next_claim` are the next unassigned Route ID and
/// Announce ID.
pub(crate) fn plan(
	old: &BTreeSet<Path>,
	new: &BTreeSet<Path>,
	added: &[Path],
	routes: &BTreeMap<u64, Route>,
	mut next_route: u64,
	mut next_claim: u64,
) -> Plan {
	let region = |prefix: &Path| ancestors(prefix).find(|above| new.contains(above));
	let mut stays: BTreeMap<(route::Id, Path), u64> = BTreeMap::new();
	let mut widening: BTreeMap<(route::Id, Path), Vec<u64>> = BTreeMap::new();
	let mut narrowing: Vec<(u64, Path)> = Vec::new();
	for (id, route) in routes {
		debug_assert!(old.contains(&route.region), "a route lies in a requested prefix");
		let covered = region(&route.prefix).is_some();
		// A route at its own prefix stays there, as one strictly inside the region does.
		if covered && (new.contains(&route.region) || route.prefix != route.region || route.exact) {
			stays.insert((route.node, route.prefix.clone()), *id);
		} else if let Some(wider) = region(&route.region) {
			widening.entry((route.node, wider)).or_default().push(*id);
		} else if route.prefix == route.region {
			let inside = (added.iter()).filter(|inside| **inside != route.region && within(inside, &route.region));
			narrowing.extend(inside.map(|inside| (*id, inside.clone())));
		}
	}

	let mut carried: BTreeMap<u64, Carried> = BTreeMap::new();
	let kept = |claims: &BTreeMap<Path, u64>| -> BTreeMap<Path, Claim> {
		let claims = claims.iter();
		claims
			.map(|(path, id)| (path.clone(), Claim { id: *id, from: *id }))
			.collect()
	};
	for ((_, wider), sources) in widening {
		let winner = (sources.iter().copied())
			.min_by_key(|id| (routes[id].order, *id))
			.expect("a key routes widen into");
		let mut claims = BTreeMap::new();
		for id in &sources {
			let route = &routes[id];
			claims.extend(
				kept(&route.claims)
					.into_iter()
					.filter(|(path, _)| *path != route.prefix),
			);
		}
		let route = Carried {
			id: winner,
			from: winner,
			kind: Kind::Widens,
			prefix: wider.clone(),
			region: wider,
			exact: false,
			claims,
		};
		carried.insert(winner, route);
	}
	for (from, inside) in narrowing {
		let route = &routes[&from];
		let key = (route.node, inside.clone());
		// A route already there stays unless this one would be preferred.
		if let Some(staying) = stays.get(&key) {
			if routes[staying].order <= route.order {
				continue;
			}
			stays.remove(&key);
		}
		let mut claims: BTreeMap<Path, Claim> = (route.claims.iter())
			.filter(|(path, _)| **path != inside && within(path, &inside))
			.map(|(path, id)| (path.clone(), Claim { id: *id, from: *id }))
			.collect();
		// Of the claims covering the whole prefix alike, only the most specific shows.
		let above = (route.claims.iter()).filter(|(path, _)| within(&inside, path));
		if let Some((path, from)) = above.max_by_key(|(path, _)| path.parts().count()) {
			let id = match *path == inside {
				true => *from,
				false => {
					next_claim += 1;
					next_claim - 1
				}
			};
			claims.insert(inside.clone(), Claim { id, from: *from });
		}
		let id = next_route;
		next_route += 1;
		let route = Carried {
			id,
			from,
			kind: Kind::Narrows,
			prefix: inside.clone(),
			region: inside,
			exact: false,
			claims,
		};
		carried.insert(id, route);
	}
	for ((_, prefix), id) in stays {
		let route = &routes[&id];
		let route = Carried {
			id,
			from: id,
			kind: Kind::Stays,
			region: region(&prefix).expect("a route that stays is covered"),
			prefix,
			exact: route.exact,
			claims: kept(&route.claims),
		};
		carried.insert(id, route);
	}
	Plan {
		routes: carried.into_values().collect(),
		next_route,
		next_claim,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn path(path: &str) -> Path {
		Path::new(path).unwrap()
	}

	fn set(paths: &[&str]) -> BTreeSet<Path> {
		paths.iter().map(|p| path(p)).collect()
	}

	const BEST: Order = (false, false, (false, Reverse(0), 1));
	const HELD: Order = (true, false, (false, Reverse(0), 1));

	fn route(node: route::Id, prefix: &str, region: &str, order: Order, claims: &[(&str, u64)]) -> Route {
		Route {
			node,
			prefix: path(prefix),
			region: path(region),
			exact: false,
			order,
			claims: claims.iter().map(|(p, id)| (path(p), *id)).collect(),
		}
	}

	fn ids(plan: &Plan) -> Vec<(u64, u64, Kind, String)> {
		let routes = plan.routes.iter();
		routes.map(|r| (r.id, r.from, r.kind, r.prefix.to_string())).collect()
	}

	#[test]
	fn widening_merges_and_keeps_ids() {
		let routes = BTreeMap::from([
			(0, route(7, "a/b", "a/b", HELD, &[("a/b", 0), ("a/b/x", 1)])),
			(1, route(7, "a/c", "a/c", BEST, &[("a/c/y", 2)])),
			(2, route(7, "a/c/deep", "a/c", BEST, &[("a/c/deep/z", 3)])),
			(3, route(9, "q", "q", BEST, &[("q/k", 4)])),
		]);
		let plan = plan(&set(&["a/b", "a/c", "q"]), &set(&["a"]), &[path("a")], &routes, 4, 5);
		assert_eq!(
			ids(&plan),
			vec![
				(1, 1, Kind::Widens, "a".to_string()),
				(2, 2, Kind::Stays, "a/c/deep".to_string())
			]
		);
		// The claim at the route's own prefix goes; the loser's claims move in.
		let claims: Vec<_> = plan.routes[0]
			.claims
			.iter()
			.map(|(p, c)| (p.to_string(), c.id))
			.collect();
		assert_eq!(claims, vec![("a/b/x".to_string(), 1), ("a/c/y".to_string(), 2)]);
		assert_eq!(plan.routes[1].region, path("a"));
		assert_eq!((plan.next_route, plan.next_claim), (4, 5));
	}

	#[test]
	fn narrowing_presents_the_covering_claim() {
		let routes = BTreeMap::from([
			(
				0,
				route(7, "a", "a", BEST, &[("a", 0), ("a/b/x", 1), ("a/c", 2), ("a/d", 3)]),
			),
			(1, route(7, "a/c", "a", HELD, &[("a/c/y", 4)])),
		]);
		let added = [path("a/c"), path("a/b")];
		let plan = plan(&set(&["a"]), &set(&["a/b", "a/c"]), &added, &routes, 2, 5);
		// The narrowed route is preferred to the held one already at `a/c`.
		assert_eq!(
			ids(&plan),
			vec![
				(2, 0, Kind::Narrows, "a/c".to_string()),
				(3, 0, Kind::Narrows, "a/b".to_string())
			]
		);
		assert_eq!(
			plan.routes[0].claims,
			BTreeMap::from([(path("a/c"), Claim { id: 2, from: 2 })])
		);
		assert_eq!(
			plan.routes[1].claims,
			BTreeMap::from([
				(path("a/b"), Claim { id: 5, from: 0 }),
				(path("a/b/x"), Claim { id: 1, from: 1 })
			])
		);
		assert_eq!((plan.next_route, plan.next_claim), (4, 6));
	}

	#[test]
	fn a_staying_route_keeps_its_key_against_an_equal() {
		let routes = BTreeMap::from([
			(0, route(7, "a", "a", BEST, &[("a/c/y", 1)])),
			(1, route(7, "a/c", "a", BEST, &[("a/c/z", 2)])),
			(2, route(7, "b", "b", BEST, &[])),
		]);
		let plan = plan(&set(&["a", "b"]), &set(&["a/c", "b"]), &[path("a/c")], &routes, 3, 3);
		assert_eq!(
			ids(&plan),
			vec![
				(1, 1, Kind::Stays, "a/c".to_string()),
				(2, 2, Kind::Stays, "b".to_string())
			]
		);
	}

	/// A route at its own prefix stays there when an edit widens the prefix it was
	/// requested under, keeping its id, claims, and floor, while a route presented there
	/// because it covers it widens. Narrowed, it is presented below its own prefix.
	#[test]
	fn an_exact_route_stays_at_its_own_prefix() {
		let exact = |route: Route| Route { exact: true, ..route };
		let routes = BTreeMap::from([
			(0, exact(route(7, "a/b", "a/b", BEST, &[("a/b", 0), ("a/b/x", 1)]))),
			(1, route(8, "a/b", "a/b", BEST, &[("a/b", 2), ("a/b/y", 3)])),
		]);
		let planned = plan(&set(&["a/b"]), &set(&["a"]), &[path("a")], &routes, 2, 4);
		assert_eq!(
			ids(&planned),
			vec![
				(0, 0, Kind::Stays, "a/b".to_string()),
				(1, 1, Kind::Widens, "a".to_string())
			]
		);
		let stays = &planned.routes[0];
		assert_eq!((stays.region.clone(), stays.exact), (path("a"), true));
		assert_eq!(
			stays.claims,
			BTreeMap::from([
				(path("a/b"), Claim { id: 0, from: 0 }),
				(path("a/b/x"), Claim { id: 1, from: 1 })
			])
		);
		assert!(!planned.routes[1].exact);

		// Asked at its own prefix again, it lies there exactly once more.
		let routes = BTreeMap::from([(
			0,
			Route {
				region: path("a"),
				..exact(route(7, "a/b", "a", BEST, &[]))
			},
		)]);
		let planned = plan(&set(&["a"]), &set(&["a/b"]), &[path("a/b")], &routes, 2, 4);
		assert_eq!(ids(&planned), vec![(0, 0, Kind::Stays, "a/b".to_string())]);
		assert_eq!(
			(planned.routes[0].region.clone(), planned.routes[0].exact),
			(path("a/b"), true)
		);

		// Narrowed, it covers the narrower prefix from its own.
		let routes = BTreeMap::from([(0, exact(route(7, "a/b", "a/b", BEST, &[("a/b", 0)])))]);
		let planned = plan(&set(&["a/b"]), &set(&["a/b/c"]), &[path("a/b/c")], &routes, 1, 1);
		assert_eq!(ids(&planned), vec![(1, 0, Kind::Narrows, "a/b/c".to_string())]);
		assert!(!planned.routes[0].exact);
	}
}
