use std::{
    convert::Infallible,
    pin::Pin,
    task::{Context, Poll},
};

use hyper::body::{Body, Bytes, Frame, SizeHint};
use tracing::trace;

/// A body that continually uploads a chunk of the same bytes until
/// it has sent a given number of bytes.
#[derive(Debug)]
pub struct UploadBody {
    remaining: usize,
    chunk: Bytes,
}

impl UploadBody {
    pub fn new(size: usize) -> Self {
        const CHUNK_SIZE: usize = 256 * 1024; // 1MB

        UploadBody {
            remaining: size,
            chunk: vec![0x55; std::cmp::min(CHUNK_SIZE, size)].into(),
        }
    }
}

impl Body for UploadBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        trace!(
            "upload body poll_frame: remaing={}, chunk.len={}",
            self.remaining,
            self.chunk.len()
        );

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

    fn is_end_stream(&self) -> bool {
        self.remaining == 0
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.remaining as u64)
    }
}
