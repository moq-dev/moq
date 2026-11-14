//! Records a [`Sim`] run under the path rule as a trace of
//! `rs/moq-net/verify/route/RouteHandoff.tla`, for TLC to check that the run is a
//! behaviour of the model (`rs/moq-net/verify/route/trace/RouteTrace.tla`).
//!
//! A trace is newline-delimited JSON. The first line names the model's constants:
//! the nodes it has, the publisher, the links with their costs, and the claims.
//! Every later line is one step: the model action it is (`op`), the messages it sent
//! between modelled nodes, and what each node it touched now routes and shows.
//!
//! The model has one publisher and relays that pass routes on, so a trace keeps
//! only those nodes. A leaf that publishes nothing, like a client, offers no route,
//! so no path crosses it and nothing it does reaches another node's routes: the
//! trace drops it, and steps that only it takes are `skip`, a step the model does not
//! see, which must send nothing between modelled nodes. Messages to the publisher are
//! dropped too, as the model drops them: it ignores what it hears.
//!
//! [`Sim::trace`] starts recording when `MOQ_ROUTE_TRACE` names a directory, and
//! [`Sim::finish_trace`] writes the trace there. `MOQ_ROUTE_TRACE_FROM` skips the
//! scenarios before it, so a run can trace a window of a long sweep.
//! `test/route-trace.sh` records the sweeps and has TLC check every trace.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::*;

/// A run being recorded.
pub(super) struct Recorder {
	file: PathBuf,
	/// The publisher's identity, and the node publishing it.
	id: Id,
	origin: Node,
	/// The nodes the trace keeps: the publisher and every relay.
	nodes: BTreeSet<Node>,
	claims: BTreeSet<Path>,
	/// The model's attributes for each item announced: the revision that set it.
	items: BTreeMap<(Path, Item), Rev>,
	/// The messages sent since the last step.
	sent: Vec<Value>,
	steps: Vec<Value>,
}

impl Sim {
	/// Record the rest of this run as `<sweep>-<scenario>` if `MOQ_ROUTE_TRACE` names a
	/// directory. `origin` publishes the one identity, and `claims` are the claims it
	/// may make: the trace starts here, as the model's initial state does, with every
	/// configured link open, the publisher's identity declared with no claims, and
	/// nothing in flight.
	pub fn trace(&mut self, sweep: &str, scenario: usize, origin: Node, claims: &[&str]) {
		let Some(dir) = std::env::var_os("MOQ_ROUTE_TRACE") else {
			return;
		};
		let from = std::env::var("MOQ_ROUTE_TRACE_FROM").map_or(0, |from| from.parse().expect("MOQ_ROUTE_TRACE_FROM"));
		if !self.paths() || self.rule != Rule::PATH || scenario < from {
			return;
		}
		let id = self.proven(origin);
		assert!(self.busy().is_empty(), "a trace starts with nothing in flight");
		assert_eq!(self.revision(id), 0, "a trace starts with no claims");
		let nodes = (0..self.nodes.len())
			.filter(|&node| node == origin || !self.leaf[node])
			.collect();
		self.trace = Some(Recorder {
			file: PathBuf::from(dir).join(format!("{sweep}-{scenario}.ndjson")),
			id,
			origin,
			nodes,
			claims: claims.iter().map(|claim| p(claim)).collect(),
			items: BTreeMap::new(),
			sent: Vec::new(),
			steps: Vec::new(),
		});
		for &(a, b) in &self.up {
			assert!(
				!(self.modelled(a) && self.modelled(b)) || self.generations[&(a, b)] == 0,
				"a trace starts on sessions never reopened"
			);
		}
	}

	/// Write the recorded trace, if any.
	pub fn finish_trace(&mut self) {
		let Some(trace) = self.trace.take() else {
			return;
		};
		assert!(trace.sent.is_empty(), "messages sent outside a step");
		let links: Vec<Value> = self
			.up
			.iter()
			.filter(|(a, b)| trace.nodes.contains(a) && trace.nodes.contains(b))
			.map(|&(a, b)| {
				let cost = self.links[a][b].expect("configured").cost;
				assert_eq!(cost, self.links[b][a].expect("configured").cost, "a symmetric cost");
				json!([a, b, cost])
			})
			.collect();
		let header = json!({
			"nodes": trace.nodes,
			"origin": trace.origin,
			"links": links,
			"claims": trace.claims.iter().map(Path::as_str).collect::<Vec<_>>(),
		});
		let mut out = String::new();
		for line in std::iter::once(&header).chain(&trace.steps) {
			out += &line.to_string();
			out.push('\n');
		}
		std::fs::write(&trace.file, out).expect("write the trace");
	}

	/// Fail a run that does something the trace cannot express.
	pub(super) fn untraced(&self, what: &str) {
		assert!(self.trace.is_none(), "a traced run cannot {what}");
	}

	fn modelled(&self, node: Node) -> bool {
		self.trace.as_ref().is_some_and(|trace| trace.nodes.contains(&node))
	}

	/// Record a message `from` queued for `to`, if the model has it.
	pub(super) fn note_sent(&mut self, from: Node, to: Node, msg: &Msg) {
		let Some(trace) = &self.trace else {
			return;
		};
		if !self.modelled(from) || !self.modelled(to) || to == trace.origin {
			return;
		}
		let key = |id: Id, prefix: &Path| {
			assert_eq!(id, trace.id, "one publisher");
			Self::key(prefix)
		};
		let msg = match msg {
			Msg::Path { id, prefix, reach, .. } => match reach {
				Some(path::Reach::Finite(offer)) => json!({
					"t": "offer",
					"k": key(*id, prefix),
					"path": self.hops(&offer.path),
					"drain": offer.draining,
				}),
				Some(path::Reach::Held) => json!({"t": "hold", "k": key(*id, prefix)}),
				None => json!({"t": "end", "k": key(*id, prefix)}),
			},
			Msg::Claims { id, prefix, floor, .. } => json!({
				"t": "claims",
				"k": key(*id, prefix),
				"rev": floor.map_or(-1, |floor| floor as i64),
			}),
			Msg::Dead(path::Dead::Closed { hop, far, moved, gone }) => {
				let named = self.hop_of(*hop);
				assert_eq!(
					named[1],
					Self::node_of(*far) as u64,
					"a DEAD's far end is its hop's peer"
				);
				let moved: Vec<Value> = moved
					.iter()
					.map(|moved| {
						json!([
							key(moved.id, &moved.prefix),
							self.hops(&moved.offer.path),
							moved.offer.draining
						])
					})
					.collect();
				json!({"t": "dead", "hop": named, "moved": moved, "gone": gone})
			}
			Msg::Dead(path::Dead::Draining { node }) => json!({"t": "drain", "node": Self::node_of(*node)}),
			Msg::Synced { .. } => return,
			msg => panic!("the path rule sent {msg:?}"),
		};
		let sent = json!([from, to, msg]);
		self.trace.as_mut().unwrap().sent.push(sent);
	}

	/// The model's key for a route's prefix: the root, or the prefix itself.
	fn key(prefix: &Path) -> String {
		match prefix.as_str() {
			"" => "root".to_string(),
			prefix => prefix.to_string(),
		}
	}

	/// The node a hop id names, which never restarted in a traced run.
	fn node_of(id: Id) -> Node {
		assert!((1_000..2_000).contains(&id), "{id} names a node that never restarted");
		(id - 1_000) as Node
	}

	/// The model's hop: the node, the neighbour it learned the route from, and that
	/// session's generation. A publisher names itself with generation 0.
	fn hop_of(&self, hop: path::Hop) -> [u64; 3] {
		let node = Self::node_of(hop.node);
		if hop.session == 0 {
			return [node as u64, node as u64, 0];
		}
		let (peer, generation) = self.numbers[&(node, hop.session)];
		[node as u64, peer as u64, u64::from(generation)]
	}

	fn hops(&self, path: &[path::Hop]) -> Vec<[u64; 3]> {
		path.iter().map(|hop| self.hop_of(*hop)).collect()
	}

	/// What `node` routes and shows, as the model keeps it.
	fn node_state(&self, node: Node) -> Value {
		let trace = self.trace.as_ref().unwrap();
		let router = self.path(node);
		let route = match router.route_for(trace.id, &Path::empty()) {
			None => json!({"on": false}),
			Some(path::Route::Live { next, offer }) => json!({
				"on": true,
				"held": false,
				"next": next,
				"path": self.hops(&offer.path),
				"drain": offer.draining,
			}),
			Some(path::Route::Held { under, path }) => json!({
				"on": true,
				"held": true,
				"next": under.map_or(-1, |under| under as i64),
				"path": self.hops(path),
			}),
			Some(route) => panic!("node {node} routes {route:?} between updates"),
		};
		let others: Vec<&Path> = router
			.routes(trace.id)
			.map(|(prefix, _)| prefix)
			.filter(|prefix| !prefix.is_empty())
			.collect();
		assert!(
			others.is_empty(),
			"node {node} routes {others:?}, which the model has not"
		);
		let shown: Vec<Value> = router
			.shown()
			.iter()
			.filter(|((id, _), _)| *id == trace.id)
			.map(|((_, claim), shown)| {
				let item = trace.items[&(claim.clone(), shown.item)];
				json!([claim.as_str(), item, shown.available])
			})
			.collect();
		json!({"node": node, "route": route, "shown": shown})
	}

	/// Record a step: `op`, the messages sent since the last one, and what each
	/// modelled node in `touched` now routes and shows.
	fn note(&mut self, mut op: Value, touched: &[Node]) {
		let Some(trace) = &self.trace else {
			return;
		};
		let origin = trace.origin;
		let state: Vec<Value> = touched
			.iter()
			.copied()
			.filter(|&node| self.modelled(node) && node != origin)
			.map(|node| self.node_state(node))
			.collect();
		let trace = self.trace.as_mut().unwrap();
		op["sent"] = Value::Array(std::mem::take(&mut trace.sent));
		op["state"] = Value::Array(state);
		trace.steps.push(op);
	}

	/// A step the model does not see: it must send nothing between modelled nodes.
	pub(super) fn note_skip(&mut self, touched: &[Node]) {
		self.note(json!({"op": "skip"}), touched);
	}

	pub(super) fn note_link(&mut self, op: &str, a: Node, b: Node) {
		match self.modelled(a) && self.modelled(b) {
			true => self.note(json!({"op": op, "a": a, "b": b}), &[a, b]),
			false => self.note_skip(&[a, b]),
		}
	}

	pub(super) fn note_deliver(&mut self, from: Node, to: Node, modelled: bool) {
		let origin = self.trace.as_ref().map(|trace| trace.origin);
		match modelled && self.modelled(from) && self.modelled(to) && Some(to) != origin {
			true => self.note(json!({"op": "deliver", "from": from, "to": to}), &[to]),
			false => self.note_skip(&[to]),
		}
	}

	pub(super) fn note_goaway(&mut self, node: Node, from: Node) {
		let origin = self.trace.as_ref().map(|trace| trace.origin);
		match self.modelled(node) && self.modelled(from) && Some(node) != origin {
			true => self.note(json!({"op": "goaway", "node": node, "from": from}), &[node]),
			false => self.note_skip(&[node]),
		}
	}

	pub(super) fn note_timeout(&mut self, node: Node, (lifted, expired): (Vec<Id>, Vec<path::Hop>)) {
		if self.trace.is_none() {
			return;
		}
		let lift: Vec<Node> = (lifted.into_iter().map(Self::node_of))
			.filter(|&node| self.modelled(node))
			.collect();
		let expire: Vec<[u64; 3]> = (expired.into_iter().map(|hop| self.hop_of(hop)))
			.filter(|hop| self.modelled(hop[0] as Node) && self.modelled(hop[1] as Node))
			.collect();
		// The model's publisher keeps no doubt: it ignores what it hears.
		let origin = self.trace.as_ref().map(|trace| trace.origin);
		match self.modelled(node) && Some(node) != origin && !(lift.is_empty() && expire.is_empty()) {
			true => self.note(
				json!({"op": "timeout", "node": node, "lift": lift, "expire": expire}),
				&[node],
			),
			false => self.note_skip(&[node]),
		}
	}

	pub(super) fn note_claim(&mut self, node: Node, claim: Path, item: Option<Item>, rev: Rev) {
		let Some(trace) = &mut self.trace else {
			return;
		};
		assert_eq!(node, trace.origin, "one publisher");
		assert!(
			trace.claims.contains(&claim),
			"{claim} is not one of the trace's claims"
		);
		if let Some(item) = item {
			let was = trace.items.insert((claim.clone(), item), rev);
			assert!(was.is_none(), "each announcement in a trace sets a new item");
		}
		let op = json!({"op": "claim", "claim": claim.as_str(), "item": item.is_some(), "rev": rev});
		self.note(op, &[]);
	}
}
