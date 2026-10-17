use std::{
    fmt::{self, Debug},
    future::Future,
    io::{self, IoSliceMut},
    net::SocketAddr,
    pin::Pin,
    task::{self, Context, Poll},
    time::Duration,
};

use moq_sock::udp::{RecvMeta, Transmit};

use crate::quic::driver::Instant;

/// Abstract implementation of a UDP socket for runtime independence
pub trait AsyncUdpSocket: Send + Sync + Debug + 'static {
    /// Create a [`UdpSender`] that can register a single task for write-readiness notifications
    /// and send a transmit, if ready.
    ///
    /// A `poll_send` method on a single object can usually store only one [`Waker`] at a time,
    /// i.e. allow at most one caller to wait for an event. This method allows any number of
    /// interested tasks to construct their own [`UdpSender`] object. They can all then wait for the
    /// same event and be notified concurrently, because each [`UdpSender`] can store a separate
    /// [`Waker`].
    ///
    /// [`Waker`]: std::task::Waker
    fn create_sender(&self) -> Pin<Box<dyn UdpSender>>;

    /// Receive UDP datagrams, or register to be woken if receiving may succeed in the future
    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>>;

    /// Look up the local IP address and port used by this socket
    fn local_addr(&self) -> io::Result<SocketAddr>;

    /// Maximum number of datagrams that might be described by a single [`RecvMeta`]
    fn max_receive_segments(&self) -> usize {
        1
    }

    /// Whether datagrams might get fragmented into multiple parts
    ///
    /// Sockets should prevent this for best performance. See e.g. the `IPV6_DONTFRAG` socket
    /// option.
    fn may_fragment(&self) -> bool {
        true
    }
}

/// An object for asynchronously writing to an associated [`AsyncUdpSocket`].
///
/// Any number of [`UdpSender`]s may exist for a single [`AsyncUdpSocket`]. Each [`UdpSender`] is
/// responsible for notifying at most one task for send readiness.
pub trait UdpSender: Send + Sync + Debug + 'static {
    /// Send a UDP datagram, or register to be woken if sending may succeed in the future.
    ///
    /// Usually implementations of this will poll the socket for writability before trying to
    /// write to them, and retry both if writing fails.
    ///
    /// Quinn will create multiple [`UdpSender`]s, one for each task it's using it from. Thus it's
    /// important to poll the underlying socket in a way that doesn't overwrite wakers.
    ///
    /// A single [`UdpSender`] will be re-used, even if `poll_send` returns `Poll::Ready` once,
    /// unlike [`Future::poll`], so calling it again after readiness should not panic.
    ///
    /// An error matching [`moq_sock::udp::is_unreachable_err`] fails a handshaking connection
    /// with `NO_VIABLE_PATH` and counts as loss once it is established; any other error closes
    /// the connection.
    fn poll_send(
        self: Pin<&mut Self>,
        transmit: &Transmit<'_>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>>;

    /// Maximum number of datagrams that a [`Transmit`] may encode.
    fn max_transmit_segments(&self) -> usize {
        1
    }
}

pin_project_lite::pin_project! {
    /// A helper for constructing [`UdpSender`]s from an underlying `Socket` type.
    ///
    /// This struct implements [`UdpSender`] if `MakeWritableFn` produces a `WritableFut`.
    ///
    /// Also serves as a trick, since `WritableFut` doesn't need to be a named future,
    /// it can be an anonymous async block, as long as `MakeWritableFn` produces that
    /// anonymous async block type.
    ///
    /// The `UdpSenderHelper` generic type parameters don't need to named, as it will be
    /// used in its dyn-compatible form as a `Pin<Box<dyn UdpSender>>`.
    struct UdpSenderHelper<Socket, MakeWritableFutFn, WritableFut> {
        socket: Socket,
        last_send_error: Option<Instant>,
        make_writable_fut_fn: MakeWritableFutFn,
        #[pin]
        writable_fut: Option<WritableFut>,
    }
}

impl<Socket, MakeWritableFutFn, WritableFut> Debug
    for UdpSenderHelper<Socket, MakeWritableFutFn, WritableFut>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UdpSender")
    }
}

impl<Socket, MakeWritableFutFn, WriteableFut>
    UdpSenderHelper<Socket, MakeWritableFutFn, WriteableFut>
{
    /// Create helper that implements [`UdpSender`] from a socket.
    ///
    /// Additionally you need to provide what is essentially an async function
    /// that resolves once the socket is write-ready.
    ///
    /// See also the bounds on this struct's [`UdpSender`] implementation.
    fn new(inner: Socket, make_fut: MakeWritableFutFn) -> Self {
        Self {
            socket: inner,
            last_send_error: None,
            make_writable_fut_fn: make_fut,
            writable_fut: None,
        }
    }
}

impl<Socket, MakeWritableFutFn, WritableFut> UdpSender
    for UdpSenderHelper<Socket, MakeWritableFutFn, WritableFut>
where
    Socket: UdpSenderHelperSocket,
    MakeWritableFutFn: Fn(&Socket) -> WritableFut + Send + Sync + 'static,
    WritableFut: Future<Output = io::Result<()>> + Send + Sync + 'static,
{
    fn poll_send(
        self: Pin<&mut Self>,
        transmit: &Transmit<'_>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        let mut this = self.project();
        loop {
            if this.writable_fut.is_none() {
                this.writable_fut
                    .set(Some((this.make_writable_fut_fn)(this.socket)));
            }
            // We're forced to `unwrap` here because `Fut` may be `!Unpin`, which means we can't safely
            // obtain an `&mut WritableFut` after storing it in `self.writable_fut` when `self` is already behind `Pin`,
            // and if we didn't store it then we wouldn't be able to keep it alive between
            // `poll_send` calls.
            let result = task::ready!(this.writable_fut.as_mut().as_pin_mut().unwrap().poll(cx));

            // Polling an arbitrary `Future` after it becomes ready is a logic error, so arrange for
            // a new `Future` to be created on the next call.
            this.writable_fut.set(None);

            // If .writable() fails, propagate the error
            result?;

            match this.socket.try_send(transmit) {
                // We thought the socket was writable, but it wasn't, then retry so that either another
                // `writable().await` call determines that the socket is indeed not writable and
                // registers us for a wakeup, or the send succeeds if this really was just a
                // transient failure.
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                // No route to the destination: retransmitting cannot help, so the connection
                // decides whether to give up on the peer.
                Err(e) if moq_sock::udp::is_unreachable_err(&e) => return Poll::Ready(Err(e)),
                Err(e) => {
                    log_sendmsg_error(this.last_send_error, &e, transmit);
                    return Poll::Ready(Ok(()));
                }
                Ok(()) => return Poll::Ready(Ok(())),
            }
        }
    }

    fn max_transmit_segments(&self) -> usize {
        self.socket.max_transmit_segments()
    }
}

/// Limits I/O error logging to one message per minute.
const IO_ERROR_LOG_INTERVAL: Duration = Duration::from_secs(60);

fn log_sendmsg_error(
    last_send_error: &mut Option<Instant>,
    error: &io::Error,
    transmit: &Transmit<'_>,
) {
    #[cfg(unix)]
    // Unix `EMSGSIZE` is expected for MTU probes.
    if moq_sock::udp::is_msg_size_err(error) {
        return;
    }

    let now = Instant::now();
    if last_send_error
        .is_some_and(|last| now.saturating_duration_since(last) <= IO_ERROR_LOG_INTERVAL)
    {
        return;
    }
    *last_send_error = Some(now);

    tracing::warn!(
        "sendmsg error: {:?}, Transmit: {{ destination: {:?}, src_ip: {:?}, ecn: {:?}, len: {:?}, segment_size: {:?} }}",
        error,
        transmit.destination,
        transmit.src_ip,
        transmit.ecn,
        transmit.contents.len(),
        transmit.segment_size
    );
}

/// Parts of the [`UdpSender`] trait that aren't asynchronous or require storing wakers.
///
/// This trait is used by [`UdpSenderHelper`] to help construct [`UdpSender`]s.
trait UdpSenderHelperSocket: Send + Sync + 'static {
    /// Try to send a transmit, if the socket happens to be write-ready.
    ///
    /// If not write-ready, this is allowed to return [`std::io::ErrorKind::WouldBlock`].
    ///
    /// The [`UdpSenderHelper`] will use this to implement [`UdpSender::poll_send`].
    fn try_send(&self, transmit: &Transmit<'_>) -> io::Result<()>;

    /// See [`UdpSender::max_transmit_segments`].
    fn max_transmit_segments(&self) -> usize;
}

mod tokio;
pub(crate) use tokio::wrap_udp_socket;

#[cfg(test)]
mod tests {
    use std::{future, task::Waker};

    use super::*;

    #[test]
    fn non_would_block_send_errors_are_logged_and_ignored() {
        let mut sender = Box::pin(UdpSenderHelper::new(
            TestSocket(io::ErrorKind::PermissionDenied),
            writable,
        ));
        let mut cx = Context::from_waker(Waker::noop());

        let result = sender.as_mut().poll_send(&transmit(), &mut cx);

        assert!(matches!(result, Poll::Ready(Ok(()))));
        assert!(sender.last_send_error.is_some());
    }

    #[test]
    fn unreachable_send_errors_are_returned() {
        let mut sender = Box::pin(UdpSenderHelper::new(
            TestSocket(io::ErrorKind::NetworkUnreachable),
            writable,
        ));
        let mut cx = Context::from_waker(Waker::noop());

        let result = sender.as_mut().poll_send(&transmit(), &mut cx);

        assert!(
            matches!(result, Poll::Ready(Err(ref e)) if e.kind() == io::ErrorKind::NetworkUnreachable),
            "{result:?}"
        );
    }

    fn transmit() -> Transmit<'static> {
        Transmit {
            destination: SocketAddr::from(([127, 0, 0, 1], 4433)),
            ecn: None,
            contents: &[],
            segment_size: None,
            src_ip: None,
        }
    }

    #[derive(Debug)]
    struct TestSocket(io::ErrorKind);

    impl UdpSenderHelperSocket for TestSocket {
        fn try_send(&self, _transmit: &Transmit<'_>) -> io::Result<()> {
            Err(self.0.into())
        }

        fn max_transmit_segments(&self) -> usize {
            1
        }
    }

    fn writable(_: &TestSocket) -> future::Ready<io::Result<()>> {
        future::ready(Ok(()))
    }
}
