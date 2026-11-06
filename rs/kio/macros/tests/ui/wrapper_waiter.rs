// No waiter, two, and a mutable one.
use std::task::Poll;

struct S;

impl S {
	#[kio::wrapper]
	fn poll_none(&self) -> Poll<()> {
		Poll::Ready(())
	}

	#[kio::wrapper]
	fn poll_two(&self, _a: &kio::Waiter, _b: &kio::Waiter) -> Poll<()> {
		Poll::Ready(())
	}

	#[kio::wrapper]
	fn poll_mutable(&self, _waiter: &mut kio::Waiter) -> Poll<()> {
		Poll::Ready(())
	}
}

fn main() {}
