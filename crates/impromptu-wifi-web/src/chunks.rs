//! Bytes received from a stream in chunks, handed out in reads of any size.

use std::collections::VecDeque;

/// A FIFO of received bytes.
#[derive(Debug, Default)]
pub struct ChunkBuffer {
    bytes: VecDeque<u8>,
}

impl ChunkBuffer {
    /// Append a received chunk.
    pub fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend(chunk);
    }

    /// Move up to `out.len()` bytes into `out`, oldest first; returns how many.
    pub fn take(&mut self, out: &mut [u8]) -> usize {
        let n = out.len().min(self.bytes.len());
        for (dst, src) in out.iter_mut().zip(self.bytes.drain(..n)) {
            *dst = src;
        }
        n
    }

    /// Whether no bytes are waiting.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_order_across_partial_takes() {
        let mut b = ChunkBuffer::default();
        b.push(&[1, 2, 3]);
        b.push(&[4]);
        let mut two = [0; 2];
        assert_eq!(b.take(&mut two), 2);
        assert_eq!(two, [1, 2]);
        assert_eq!(b.take(&mut two), 2);
        assert_eq!(two, [3, 4]);
        let mut four = [0; 4];
        assert_eq!(b.take(&mut four), 0);
        assert!(b.is_empty());
    }
}
