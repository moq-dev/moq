//! The bake-off: the multicast epic's rule ([`multi::Router`]) beside today's
//! [`path::Router`] on the same meshes and failure schedules.

use super::compare::*;
use super::*;

/// The arms measured, in the report's order.
const ARMS: [Arm; 3] = [Arm::TODAY, Arm::PLAIN, Arm::SUSPECT];

/// The prototype's arms.
const MULTI: [Arm; 2] = [Arm::PLAIN, Arm::SUSPECT];

/// The prototype carries every change of the path rule's comparison on its shapes,
/// and is right whenever it settles.
#[test]
fn the_prototype_converges_on_every_shape() {
	let mut rng = XorShift(0x6d6f_7100);
	let mut meshes = vec![Mesh::ring(6), Mesh::clique(6, 1), Mesh::clique(4, 3)];
	meshes.extend((0..8).map(|_| Mesh::sparse(6, &mut rng)));
	for mesh in &meshes {
		for arm in MULTI {
			run(arm, mesh);
		}
	}
}

/// Under churn the prototype settles and is right once it does.
#[test]
fn the_prototype_settles_after_churn() {
	for arm in MULTI {
		for two_homes in [false, true] {
			churn(arm, 0..60, two_homes);
		}
	}
}
