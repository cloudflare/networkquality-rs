// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

use std::{sync::Arc, task::Poll, time::Duration};

use hyper::body::{Body, Bytes};
use tokio::sync::mpsc;
use tracing::{debug, error, trace};

use crate::{Time, Timestamp};

/// [`BodyEvent`]s are generated by a [`CountingBody`] and describe the number
/// of total bytes seen or that the body has finished.
#[derive(Debug)]
pub enum BodyEvent {
    /// The number of bytes sent by the wrapped body at the given [`Timestamp`].
    ByteCount {
        /// When the event was generated.
        at: Timestamp,
        /// The total number of bytes seen.
        total: usize,
    },
    /// The [`CountingBody`] has finished sending the body it wraps.
    Finished {
        /// When the body finished.
        at: Timestamp,
    },
}

pin_project_lite::pin_project! {
    #[allow(missing_docs)]
    pub struct CountingBody<B> {
        #[pin]
        inner: B,
        time: Arc<dyn Time>,
        last_sent: Timestamp,
        update_every: Duration,
        total: usize,
        events_tx: mpsc::UnboundedSender<BodyEvent>,
        sent_finished: bool,
    }
}

impl<B> CountingBody<B> {
    /// Create a [`CountingBody`] by wrapping the given body. Updates are sent
    /// every `update_every` duration and timestamps are taken with the given
    /// [`Arc<dyn Time>`].
    pub fn new(
        inner: B,
        update_every: Duration,
        time: Arc<dyn Time>,
    ) -> (Self, mpsc::UnboundedReceiver<BodyEvent>) {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let last_sent = time.now();

        events_tx
            .send(BodyEvent::ByteCount {
                at: last_sent,
                total: 0,
            })
            .expect("no data buffered");

        (
            Self {
                inner,
                time,
                last_sent,
                update_every,
                total: 0,
                events_tx,
                sent_finished: false,
            },
            events_rx,
        )
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

        // stop the body if there's no event sender.
        if this.events_tx.is_closed() {
            debug!("events_tx is closed, stopping");
            return Poll::Ready(None);
        }

        trace!("polling frame");
        match this.inner.poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    *this.total += data.len();
                }

                let now = this.time.now();

                // We've waited long enough, send an update.
                if now.duration_since(*this.last_sent) >= *this.update_every {
                    let event = BodyEvent::ByteCount {
                        at: now,
                        total: *this.total,
                    };

                    *this.last_sent = now;

                    debug!(?event, "sending event");

                    // We can drop the error here since this is an
                    // increasing counter. The next send will hopefully
                    // capture it.
                    let _ = this.events_tx.send(event);
                }

                Poll::Ready(Some(Ok(frame)))
            }
            // Stream finished, send the last count
            Poll::Ready(None) => {
                let now = this.time.now();

                debug!("body finished");
                let event = BodyEvent::ByteCount {
                    at: now,
                    total: *this.total,
                };

                debug!(?event, "sending event");
                let _ = this.events_tx.send(event);

                if !*this.sent_finished {
                    debug!(at=?now, "sending finished");
                    let _ = this.events_tx.send(BodyEvent::Finished { at: now });
                    *this.sent_finished = true;
                } else {
                    debug!("already sent finish");
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

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }
}
