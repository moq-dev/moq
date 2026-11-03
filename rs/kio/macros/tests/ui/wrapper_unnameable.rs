// A poll whose wrapper name is no identifier, even raw.
use std::task::Poll;

struct S;

impl S {
	#[kio::wrapper]
	fn poll_self(&self, _waiter: &kio::Waiter) -> Poll<()> {
		Poll::Ready(())
	}

	#[kio::wrapper]
	fn poll__(&self, _waiter: &kio::Waiter) -> Poll<()> {
		Poll::Ready(())
	}

	#[kio::wrapper]
	fn poll_1(&self, _waiter: &kio::Waiter) -> Poll<()> {
		Poll::Ready(())
	}
}

fn main() {}
