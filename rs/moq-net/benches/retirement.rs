//! Cost of observing replacement at a covering source, swept over unrelated routes and held requests.
//! The route-only control models the old gate: a more-specific source does not retract the broad route.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench retirement`.

use std::fmt;

use moq_net::{Path, fuzz::RequestValidity, origin};

/// Unrelated routes and requests held on the first, named `<routes>x<consumers>`.
#[derive(Clone, Copy)]
struct Case {
	routes: usize,
	consumers: usize,
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}x{}", self.routes, self.consumers)
	}
}

fn cases() -> Vec<Case> {
	[8, 32, 128]
		.into_iter()
		.flat_map(|routes| [1, 8, 32].map(|consumers| Case { routes, consumers }))
		.collect()
}

/// An origin with `routes` claimed routes and requests pending on them, `consumers` of them on the first.
struct Fixture {
	selected: bool,
	consumers: usize,
	origin: origin::Producer,
	_driver: origin::Driver,
	_routes: Vec<origin::Claim>,
	_watches: Vec<RequestValidity>,
	_controls: Vec<kio::Producer<()>>,
	pending: Vec<kio::Consumer<()>>,
}

impl Fixture {
	fn new(case: Case, selected: bool) -> Self {
		let (origin, driver) = origin::Producer::new(origin::Config::default());
		let routes: Vec<_> = (0..case.routes)
			.map(|i| {
				origin
					.claim(format!("route/{i}"), origin::RouteOptions::default())
					.unwrap()
			})
			.collect();
		let mut watches = Vec::new();
		let mut controls = Vec::new();
		let mut pending = Vec::new();
		for (i, route) in routes.iter().enumerate() {
			for reader in 0..if i == 0 { case.consumers } else { 1 } {
				if selected {
					let mut watch =
						RequestValidity::new(route, &Path::new(&format!("route/{i}/jobs/{reader}")).unwrap());
					pending.push(watch.start());
					watches.push(watch);
				} else {
					let control = kio::Producer::new(());
					pending.push(control.consume());
					controls.push(control);
				}
			}
		}
		Self {
			selected,
			consumers: case.consumers,
			origin,
			_driver: driver,
			_routes: routes,
			_watches: watches,
			_controls: controls,
			pending,
		}
	}

	/// Claims the covering source and checks which of the first route's requests it refused.
	fn replace(self) -> (Self, origin::Claim) {
		let replacement = self
			.origin
			.claim("route/0/jobs", origin::RouteOptions::default())
			.unwrap();
		let refused = self
			.pending
			.iter()
			.take(self.consumers)
			.filter(|request| request.is_closed())
			.count();
		assert_eq!(refused, if self.selected { self.consumers } else { 0 });
		(self, replacement)
	}
}

mod retirement {
	use moq_net::origin;
	use swole::bench;

	use super::{Case, Fixture, cases};

	fn selected(case: Case) -> Fixture {
		Fixture::new(case, true)
	}

	fn unselected(case: Case) -> Fixture {
		Fixture::new(case, false)
	}

	#[bench(args = cases(), setup = selected)]
	fn selection(fixture: Fixture) -> (Fixture, origin::Claim) {
		fixture.replace()
	}

	#[bench(args = cases(), setup = unselected)]
	fn route_only(fixture: Fixture) -> (Fixture, origin::Claim) {
		fixture.replace()
	}
}

fn main() {
	swole::main();
}
