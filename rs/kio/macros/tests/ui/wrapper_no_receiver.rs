// An associated function, with no receiver to borrow.
use std::task::Poll;

struct S;

impl S {
	#[kio::wrapper]
	fn poll_ready(_waiter: &kio::Waiter) -> Poll<()> {
		Poll::Ready(())
	}
}

fn main() {}
