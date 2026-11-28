// Qualifiers an `async fn` wrapper cannot carry, and an argument to the attribute.
use std::task::Poll;

struct S;

impl S {
	#[kio::wrapper]
	async fn poll_async(&self, _waiter: &kio::Waiter) -> Poll<()> {
		Poll::Ready(())
	}

	#[kio::wrapper]
	const fn poll_const(&self, _waiter: &kio::Waiter) -> Poll<()> {
		Poll::Ready(())
	}

	#[kio::wrapper]
	unsafe fn poll_unsafe(&self, _waiter: &kio::Waiter) -> Poll<()> {
		Poll::Ready(())
	}

	#[kio::wrapper(name = ready)]
	fn poll_named(&self, _waiter: &kio::Waiter) -> Poll<()> {
		Poll::Ready(())
	}
}

fn main() {}
