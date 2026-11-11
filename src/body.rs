use hyper::body::{Body, Frame, SizeHint};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::time::{sleep, Sleep};

/// Wraps a streaming body so that a stall longer than `idle` between
/// frames terminates the stream instead of holding a client connection
/// (and its upstream socket) open forever.
pub(crate) struct IdleTimeoutBody<B> {
    inner: Pin<Box<B>>,
    idle: Duration,
    timer: Option<Pin<Box<Sleep>>>,
}

impl<B> IdleTimeoutBody<B> {
    pub(crate) fn new(
        body: B,
        idle: Duration,
    ) -> Self {
        Self {
            inner: Box::pin(body),
            idle,
            timer: None,
        }
    }
}

impl<B> Body for IdleTimeoutBody<B>
    where
        B: Body<Data=bytes::Bytes> + 'static,
        B::Error: std::error::Error + Send + Sync + 'static,
{
    type Data = bytes::Bytes;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        match this.inner.as_mut().poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                // Any frame, data or trailers, counts as progress.
                this.timer = Some(Box::pin(sleep(this.idle)));
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(error))) => {
                Poll::Ready(Some(Err(Box::new(error))))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => {
                let timer = this.timer.get_or_insert_with(|| {
                    Box::pin(sleep(this.idle))
                });
                match timer.as_mut().poll(cx) {
                    Poll::Ready(()) => {
                        this.timer = None;
                        log_warn!(
                            "[BODY] Upstream response idle for {:?}; aborting stream",
                            this.idle
                        );
                        Poll::Ready(Some(Err(Box::new(
                            IdleTimeoutError(this.idle),
                        ))))
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
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
