//! Drain benchmark for the stats registry: [`stats::Registry::report`] refilling
//! one reused report, swept over broadcasts, tiers, labels, sessions and connections
//! so a cost that grows faster than the entries it reports shows up as a slope.
//!
//! Run with `cargo bench -p moq-net --bench stats`.

use std::fmt;

/// Two swept counts, named `<outer>x<inner>`.
#[derive(Clone, Copy)]
struct Case(usize, usize);

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}x{}", self.0, self.1)
	}
}

fn cases(outer: &[usize], inner: &'static [usize]) -> Vec<Case> {
	outer
		.iter()
		.flat_map(|&a| inner.iter().map(move |&b| Case(a, b)))
		.collect()
}

/// Labels, sessions per label, and connections per session, named
/// `<labels>x<sessions>/connections=<connections>`.
#[derive(Clone, Copy)]
struct Walk {
	labels: usize,
	sessions: usize,
	connections: usize,
}

impl fmt::Display for Walk {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}x{}/connections={}", self.labels, self.sessions, self.connections)
	}
}

fn walks() -> Vec<Walk> {
	[1, 100, 1_000]
		.into_iter()
		.flat_map(|labels| {
			[1, 10].into_iter().flat_map(move |sessions| {
				[0, 1, 4].map(|connections| Walk {
					labels,
					sessions,
					connections,
				})
			})
		})
		.collect()
}

mod stats {
	use futures::FutureExt;
	use moq_net::{origin, stats, transport};
	use swole::bench;

	use super::{Case, Walk, cases, walks};

	/// A registry holding one live announce entry per `(broadcast, tier)`, and the report it
	/// refills. The handles are held: dropping them would close the entries and prune them.
	struct Registry {
		registry: stats::Registry,
		report: stats::Report,
		_driver: origin::Driver,
		_publishers: Vec<origin::Route>,
		_cursors: Vec<origin::Routes>,
	}

	/// Every broadcast records under every tier.
	fn registry(Case(broadcasts, tiers): Case) -> Registry {
		let registry = stats::Registry::new(stats::Config::new());
		let (producer, driver) = origin::Producer::new(origin::Config::default());
		let publishers = (0..broadcasts)
			.map(|i| {
				producer
					.route(format!("room/{i}"), moq_net::origin::RouteOptions::default())
					.unwrap()
			})
			.collect();
		let cursors = (0..tiers)
			.map(|t| {
				let session = registry
					.tier(stats::Tier::new(moq_net::Path::new(&format!("tier{t}")).unwrap()))
					.session(moq_net::stats::SessionInfo::new(moq_net::Path::from_static("root")));
				let mut cursor = producer.consume().with_stats(session).routes();
				// Each announce the cursor replays records under its tier.
				while cursor.next().now_or_never().flatten().is_some() {}
				cursor
			})
			.collect();
		let mut report = stats::Report::default();
		registry.report(&mut report, moq_net::time::Instant::now());
		assert_eq!(
			report.traffic.len(),
			broadcasts * tiers,
			"one entry per broadcast and tier"
		);
		Registry {
			registry,
			report,
			_driver: driver,
			_publishers: publishers,
			_cursors: cursors,
		}
	}

	#[bench(args = [Case(100, 1), Case(1_000, 1), Case(100, 10), Case(1_000, 10)], setup = registry)]
	fn report(input: &mut Registry) {
		input.registry.report(&mut input.report, moq_net::time::Instant::now())
	}

	/// A registry with a request context per root and tier, and the last tier's handle.
	struct Requests {
		_registry: stats::Registry,
		_contexts: Vec<stats::Session>,
		handle: stats::Handle,
		root: String,
	}

	fn requests(Case(roots, tiers): Case) -> Requests {
		let registry = stats::Registry::new(stats::Config::new());
		let mut contexts = Vec::new();
		for tier in 0..tiers {
			let handle = registry.tier(stats::Tier::new(moq_net::Path::new(&format!("tier{tier}")).unwrap()));
			for root in 0..roots {
				let path = moq_net::Path::new(&format!("room/{root}")).unwrap();
				contexts.push(handle.request(stats::SessionInfo::new(path.clone()).with_root(path)));
			}
		}
		let handle = registry.tier(stats::Tier::new(
			moq_net::Path::new(&format!("tier{}", tiers - 1)).unwrap(),
		));
		Requests {
			_registry: registry,
			_contexts: contexts,
			handle,
			root: format!("room/{}", roots - 1),
		}
	}

	/// Drops the request context inside the call, as a request that ends does.
	#[bench(args = cases(&[1, 100, 1000], &[1, 10]), setup = requests)]
	fn request(input: &Requests) {
		let path = moq_net::Path::new(&input.root).unwrap();
		drop(std::hint::black_box(
			input
				.handle
				.request(stats::SessionInfo::new(path.clone()).with_root(path)),
		));
	}

	mod write {
		use moq_net::{fuzz, stats};
		use swole::bench;

		fn path() -> (stats::Registry, fuzz::Meter) {
			let registry = stats::Registry::new(stats::Config::new());
			let meter = fuzz::path_meter(&registry, &stats::Tier::default(), "room/live");
			(registry, meter)
		}

		#[bench(setup = path)]
		fn path_bytes((_, meter): &(stats::Registry, fuzz::Meter)) {
			meter.bytes(1)
		}

		fn session() -> (stats::Registry, fuzz::Meter) {
			let registry = stats::Registry::new(stats::Config::new());
			let meter = fuzz::session_meter(&registry, stats::Tier::default(), "root", "room/live");
			(registry, meter)
		}

		#[bench(setup = session)]
		fn session_bytes((_, meter): &(stats::Registry, fuzz::Meter)) {
			meter.bytes(1)
		}
	}

	/// A registry with sessions on every label, each with live connections, and the report
	/// it refills.
	struct Sessions {
		registry: stats::Registry,
		report: stats::Report,
		_contexts: Vec<stats::Session>,
		_wires: Vec<stats::Wire>,
	}

	fn sessions(walk: Walk) -> Sessions {
		let Walk {
			labels,
			sessions,
			connections,
		} = walk;
		let registry = stats::Registry::new(stats::Config::new());
		let handle = registry.tier(stats::Tier::default());
		let contexts: Vec<_> = (0..labels)
			.flat_map(|label| {
				let handle = handle.clone();
				(0..sessions).map(move |_| {
					handle.session(moq_net::stats::SessionInfo::new(
						moq_net::Path::new(&format!("project/key/{label}")).unwrap(),
					))
				})
			})
			.collect();
		let wires = contexts
			.iter()
			.flat_map(|session| {
				(0..connections).map(move |_| {
					let counters = transport::Wire::new();
					counters.set_sent_bytes(100);
					counters.set_received_bytes(200);
					session.wire(counters)
				})
			})
			.collect();
		let mut report = stats::Report::default();
		registry.report(&mut report, moq_net::time::Instant::now());
		assert_eq!(report.sessions.len(), labels);
		for row in &report.sessions {
			assert_eq!(row.presence.sessions_started, sessions as u64);
			assert_eq!(row.presence.wire_tx, (sessions * connections * 100) as u64);
			assert_eq!(row.presence.wire_rx, (sessions * connections * 200) as u64);
		}
		Sessions {
			registry,
			report,
			_contexts: contexts,
			_wires: wires,
		}
	}

	/// Include the live transport walk, independently swept over labels, sessions and connections.
	#[bench(args = walks(), setup = sessions, items = |walk: Walk| walk.labels * walk.sessions)]
	fn session_report(input: &mut Sessions) {
		input.registry.report(&mut input.report, moq_net::time::Instant::now())
	}

	/// Sessions on every label, the last label's moving.
	struct Retag {
		_registry: stats::Registry,
		tier: stats::Tier,
		_contexts: Vec<stats::Session>,
		moving: Vec<stats::Session>,
		labels: usize,
	}

	fn retagging(Case(labels, sessions): Case) -> Retag {
		let registry = stats::Registry::new(stats::Config::new());
		let tier = stats::Tier::default();
		let handle = registry.tier(tier.clone());
		let mut contexts: Vec<_> = (0..labels)
			.flat_map(|label| {
				let handle = handle.clone();
				(0..sessions).map(move |_| {
					handle.session(moq_net::stats::SessionInfo::new(
						moq_net::Path::new(&format!("project/key/{label}")).unwrap(),
					))
				})
			})
			.collect();
		let moving = contexts.split_off(contexts.len() - sessions);
		Retag {
			_registry: registry,
			tier,
			_contexts: contexts,
			moving,
			labels,
		}
	}

	/// Move every session on one label to another and back, as a revalidation that relabels
	/// a whole project does. Per-session cost should stay flat as sessions and labels grow.
	#[bench(args = cases(&[1, 100], &[1, 100, 10_000]), setup = retagging, items = |Case(_, sessions)| sessions * 2)]
	fn retag(input: &Retag) {
		for session in input.moving.iter().rev() {
			session.retag(input.tier.clone(), moq_net::Path::from_static("project/moved"));
		}
		for session in input.moving.iter().rev() {
			session.retag(
				input.tier.clone(),
				moq_net::Path::new(&format!("project/key/{}", input.labels - 1)).unwrap(),
			);
		}
	}
}

fn main() {
	swole::main();
}
