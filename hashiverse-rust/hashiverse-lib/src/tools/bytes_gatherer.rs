//! # Scatter-gather buffer for zero-copy response assembly
//!
//! The single export from this module — [`BytesGatherer`] — is how large payloads (post
//! bundles, feedback bundles) are threaded from storage, through the protocol layer,
//! through compression, and on to the transport, without ever being memcpy'd by the
//! application.
//!
//! Small POD fields (RPC headers, counters, signatures) accumulate into an internal
//! `BytesMut` scratchpad; large already-refcounted `Bytes` blobs attach by reference with
//! [`BytesGatherer::put_bytes`]. At the transport boundary [`BytesGatherer::compact`]
//! merges adjacent small segments into one and [`BytesGatherer::finish`] yields a
//! `Vec<Bytes>` suitable for vectored I/O or HTTP/2 DATA frames.
//!
//! See the doc comment on [`BytesGatherer`] for the motivating `GetPostBundle` example
//! and the on-the-wire segment layout.

use bytes::{BufMut, Bytes, BytesMut};

/// A scatter-gather buffer that threads large blobs through the response pipeline by reference,
/// eliminating application-layer copies for bulk data such as post bundles.
///
/// # Motivation
///
/// When serving a `GetPostBundleV1` response the post bundle payload (potentially hundreds of KB,
/// already brotli-compressed on disk) was previously copied multiple times before reaching the network.
/// Then at the transport boundary the single `Bytes` was handed to HTTPS/TCP with no opportunity
/// for vectored writes.
///
/// `BytesGatherer` fixes this by threading large blobs through the entire response path **by
/// reference**. Small POD fields (header bytes, JSON metadata) accumulate into an internal
/// `BytesMut` scratchpad, while large payloads are attached with [`push_bytes`] — a refcount bump
/// only, no memcpy. At the transport boundary [`compact`] merges any adjacent small segments, sets
/// `Content-Length`, and HTTP/2 receives a stream of `Bytes` chunks as separate DATA frames.
///
/// **Net result for `GetPostBundle`, the hottest-path of Hashiverse: ** multiple application-layer copies of the post bundle → 0. One
/// kernel copy to the network card remains (unavoidable).
///
/// After `compact(1024)` the segment layout for a typical `GetPostBundle` response looks like:
///
/// ```text
/// Segment 0: [rpc_header (~300 B) + response_header (peers/token/counts) (~50 B) + flag (1 B)]
///            ← merged by compact, one BytesMut flush
/// Segment 1: post_bundle Bytes (e.g. 500 KB)
///            ← original Bytes from disk, zero copy
/// ```
///
/// HTTP/2 sends these as 2 DATA frames. TCP concatenates to 1 contiguous buffer via [`to_bytes`].
///
/// [`push_bytes`]: BytesGatherer::put_bytes
/// [`compact`]: BytesGatherer::compact
/// [`to_bytes`]: BytesGatherer::to_bytes
#[derive(Default)]
pub struct BytesGatherer {
    segments: Vec<Bytes>,
    current: BytesMut,
}

impl BytesGatherer {
    pub fn from_bytes(bytes: Bytes) -> Self {
        let mut bytes_gatherer = Self::default();
        bytes_gatherer.put_bytes(bytes);
        bytes_gatherer
    }

    // Small POD fields → accumulate into current BytesMut
    pub fn put_u8(&mut self, v: u8) {
        self.current.put_u8(v);
    }
    pub fn put_u16_le(&mut self, v: u16) {
        self.current.put_u16_le(v);
    }
    pub fn put_u16(&mut self, v: u16) {
        self.current.put_u16(v);
    }
    pub fn put_u32_le(&mut self, v: u32) {
        self.current.put_u32_le(v);
    }
    pub fn put_u32(&mut self, v: u32) {
        self.current.put_u32(v);
    }
    pub fn put_u64(&mut self, v: u64) {
        self.current.put_u64(v);
    }
    pub fn put_slice(&mut self, s: &[u8]) {
        self.current.put_slice(s);
    }

    /// Push a large blob — flush accumulator first, store blob by reference (zero copy).
    pub fn put_bytes(&mut self, bytes: Bytes) {
        if bytes.is_empty() {
            return;
        }

        self.flush();
        self.segments.push(bytes);
    }

    /// Merge another gatherer — zero copy for blobs; fast path if both are still assembling.
    ///
    /// If `other` has no committed segments yet (all bytes are still in its scratchpad),
    /// the scratchpad bytes are appended directly into `self.current` — no flush, no new segment.
    pub fn put_bytes_gatherer(&mut self, mut other: BytesGatherer) {
        if other.segments.is_empty() {
            // Fast path: other is still assembling — absorb its scratchpad into ours.
            if !other.current.is_empty() {
                self.current.put_slice(&other.current);
            }
            return;
        }

        other.flush();
        self.flush();
        self.segments.extend(other.segments);
    }

    /// Returns `true` if the gatherer contains no bytes.
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty() && self.current.is_empty()
    }

    /// Total byte count across all segments + current accumulator.
    pub fn len(&self) -> usize {
        self.segments.iter().map(|b| b.len()).sum::<usize>() + self.current.len()
    }

    /// Merge adjacent segments smaller than `threshold` bytes into single segments.
    /// Large segments pass through untouched (zero copy).
    /// Generally, gall this at the transport boundary before streaming to optimise streaming window fragmentation.
    pub fn compact(mut self, threshold: usize) -> Self {
        self.flush();
        let mut out: Vec<Bytes> = Vec::with_capacity(self.segments.len());
        let mut acc = BytesMut::new();
        for seg in self.segments {
            if seg.len() < threshold {
                acc.extend_from_slice(&seg);
            }
            else {
                if !acc.is_empty() {
                    out.push(acc.split().freeze());
                }
                out.push(seg); // large — zero copy
            }
        }
        if !acc.is_empty() {
            out.push(acc.freeze());
        }
        Self { segments: out, current: BytesMut::new() }
    }

    /// Consume into `Vec<Bytes>` for streaming / vectored I/O.
    pub fn finish(mut self) -> Vec<Bytes> {
        self.flush();
        self.segments
    }

    /// Consume into one contiguous Bytes.
    /// Zero-copy fast path: single segment returned directly.
    pub fn to_bytes(mut self) -> Bytes {
        self.flush();
        if self.segments.len() == 1 {
            return self.segments.remove(0);
        }
        let total = self.segments.iter().map(|b| b.len()).sum();
        let mut out = BytesMut::with_capacity(total);
        for seg in self.segments {
            out.extend_from_slice(&seg);
        }
        out.freeze()
    }

    fn flush(&mut self) {
        if !self.current.is_empty() {
            self.segments.push(self.current.split().freeze());
        }
    }
}
