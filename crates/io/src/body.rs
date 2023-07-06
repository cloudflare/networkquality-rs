use std::{
    convert::Infallible,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use http::{HeaderMap, HeaderValue};
use hyper::body::{Body, Bytes, Frame, SizeHint};
use tokio::{
    sync::mpsc::{self, Receiver},
    time::Instant,
};
use tracing::{debug, error, trace};

use crate::{connection::ConnectionTiming, ConnectionId};

#[derive(Debug)]
pub enum BodyEvent {
    ByteCount { at: Instant, total: usize },
    Finished { at: Instant },
}

pin_project_lite::pin_project! {
    pub struct CountingBody<B> {
        #[pin]
        inner: B,
        last_sent: Instant,
        update_every: Duration,
        poll_count: u8,
        total: usize,
        events_tx: Option<mpsc::Sender<BodyEvent>>,
    }
}

impl<B> CountingBody<B> {
    pub fn new(inner: B, update_every: Duration) -> Self {
        let now = Instant::now();

        Self {
            inner,
            last_sent: now,
            update_every,
            poll_count: 0,
            total: 0,
            events_tx: None,
        }
    }

    pub fn subscribe(&mut self) -> Receiver<BodyEvent> {
        let (events_tx, events_rx) = mpsc::channel(1024);
        self.events_tx = Some(events_tx);
        events_rx
    }

    pub fn set_sender(&mut self, events_tx: mpsc::Sender<BodyEvent>) {
        self.events_tx = Some(events_tx);
    }
}

impl<B> Body for CountingBody<B>
where
    B: Body<Data = Bytes>,
    B::Error: std::fmt::Debug,
{
    type Data = B::Data;

    type Error = B::Error;

    #[inline(always)]
    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        let this = self.project();

        trace!("polling frame");
        match this.inner.poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    *this.total += data.len();
                }

                // Only check elapsed time every 5 frames
                if *this.poll_count >= 5 {
                    let now = Instant::now();

                    // We've waited long enough, send an update.
                    if now.duration_since(*this.last_sent) >= *this.update_every {
                        // We can drop the error here since this is an
                        // increasing counter. The next send will hopefully
                        // capture it.
                        if let Some(tx) = this.events_tx {
                            let event = BodyEvent::ByteCount {
                                at: now,
                                total: *this.total,
                            };

                            debug!(?event, "sending event");
                            let _ = tx.try_send(BodyEvent::ByteCount {
                                at: now,
                                total: *this.total,
                            });
                        }
                    }

                    *this.poll_count = 0;
                } else {
                    *this.poll_count += 1;
                }

                Poll::Ready(Some(Ok(frame)))
            }
            // Stream finished, send the last count
            Poll::Ready(None) => {
                let now = Instant::now();

                debug!("body finished");
                if let Some(tx) = this.events_tx {
                    let event = BodyEvent::ByteCount {
                        at: now,
                        total: *this.total,
                    };

                    debug!(?event, "sending event");
                    let _ = tx.try_send(BodyEvent::ByteCount {
                        at: now,
                        total: *this.total,
                    });

                    debug!(at=?now, "sending finished");
                    let _ = tx.try_send(BodyEvent::Finished { at: now });
                }

                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(e))) => {
                error!(error=?e, "body errored");
                Poll::Ready(Some(Err(e)))
            }
            Poll::Pending => {
                trace!("body pending");
                Poll::Pending
            }
        }
    }
}

pub struct InflightBody {
    pub start: Instant,
    pub conn_id: ConnectionId,
    pub connection_timing: Option<ConnectionTiming>,
    pub events: mpsc::Receiver<BodyEvent>,
    pub headers: HeaderMap<HeaderValue>,
}

/// A body that continually uploads a chunk of the same bytes until
/// it has sent a given number of bytes.
#[derive(Debug)]
pub struct DummyBody {
    remaining: usize,
    chunk: Bytes,
}

impl DummyBody {
    pub fn new(size: usize) -> Self {
        const CHUNK_SIZE: usize = 10 * 1024 * 1024; // 10MB

        DummyBody {
            remaining: size,
            chunk: vec![0x55; std::cmp::min(CHUNK_SIZE, size)].into(),
        }
    }
}

impl Body for DummyBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(match self.remaining {
            0 => None,
            remaining if remaining > self.chunk.len() => {
                self.remaining -= self.chunk.len();

                Some(Ok(Frame::data(self.chunk.clone())))
            }
            remaining => {
                self.remaining = 0;

                Some(Ok(Frame::data(self.chunk.slice(..remaining))))
            }
        })
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.remaining as u64)
    }
}
