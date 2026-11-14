//! Idle-timeout wrapper for streamed response bodies.
//!
//! A slow or stalled upstream must not pin a client connection forever.
//! [`IdleTimeoutBody`] watches the gap between body frames and turns a
//! stall into a body error, which ends the response cleanly instead of
//! leaving the socket open.

use hyper::body::{Body, Frame, SizeHint};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::time::{sleep, Instant, Sleep};

pin_project_lite::pin_project! {
    /// Wraps a streaming body so that a stall longer than `idle` between
    /// frames terminates the stream instead of holding a client connection
    /// (and its upstream socket) open forever.
    ///
    /// The timer is created once and reset after every frame, so polling
    /// the body does not allocate in the hot path.
    pub(crate) struct IdleTimeoutBody<B> {
        #[pin]
        inner: B,
        #[pin]
        timer: Sleep,
        idle: Duration,
    }
}

impl<B> IdleTimeoutBody<B> {
    /// Wraps `body` and starts the idle timer immediately.
    ///
    /// The clock starts at construction, so the first frame must arrive
    /// within `idle` as well.
    pub(crate) fn new(
        body: B,
        idle: Duration,
    ) -> Self {
        Self {
            inner: body,
            timer: sleep(idle),
            idle,
        }
    }
}

impl<B> Body for IdleTimeoutBody<B>
    where
        B: Body<Data=bytes::Bytes>,
        B::Error: std::error::Error + Send + Sync + 'static,
{
    type Data = bytes::Bytes;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.project();
        match this.inner.poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                // Any frame, data or trailers, counts as progress.
                this.timer
                    .reset(Instant::now() + *this.idle);
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(error))) => {
                Poll::Ready(Some(Err(Box::new(error))))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => match this.timer.poll(cx) {
                Poll::Ready(()) => {
                    log_warn!(
                        "[BODY] Upstream response idle for {:?}; aborting stream",
                        this.idle
                    );
                    Poll::Ready(Some(Err(Box::new(
                        IdleTimeoutError(*this.idle),
                    ))))
                }
                Poll::Pending => Poll::Pending,
            },
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

#[derive(Debug)]
/// Error produced when the idle timer expires; surfaced through the body
/// so hyper can abort the response.
struct IdleTimeoutError(Duration);

impl std::fmt::Display for IdleTimeoutError {
    fn fmt(
        &self,
        formatter: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        write!(
            formatter,
            "upstream response body idle for {:?}",
            self.0
        )
    }
}

impl std::error::Error for IdleTimeoutError {}
