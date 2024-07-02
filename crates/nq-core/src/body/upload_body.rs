use bytes::BytesMut;
use std::{
    convert::Infallible,
    pin::Pin,
    task::{Context, Poll},
};

use hyper::body::{Body, Bytes, Frame, SizeHint};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tracing::trace;

/// A body that continually uploads a chunk of random bytes until
/// it has sent a given number of bytes.
#[derive(Debug)]
pub struct UploadBody {
    remaining: usize,
    chunk: Bytes,
    rng: StdRng,
}

impl UploadBody {
    pub fn new(size: usize) -> Self {
        const CHUNK_SIZE: usize = 256 * 1024; // 256 KB

        let mut rng = StdRng::from_entropy();
        let chunk_size = std::cmp::min(CHUNK_SIZE, size);
        let mut chunk = vec![0u8; chunk_size];
        rng.fill(&mut chunk[..]);

        UploadBody {
            remaining: size,
            chunk: Bytes::from(chunk),
            rng,
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
            "upload body poll_frame: remaining={}, chunk.len={}",
            self.remaining,
            self.chunk.len()
        );

        Poll::Ready(match self.remaining {
            0 => None,
            remaining if remaining > self.chunk.len() => {
                self.remaining -= self.chunk.len();
                // Use BytesMut for in-place modifications
                let mut chunk = BytesMut::with_capacity(self.chunk.len());
                chunk.resize(self.chunk.len(), 0);
                self.rng.fill(&mut chunk[..]);
                // Convert to Bytes
                self.chunk = chunk.freeze();
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
