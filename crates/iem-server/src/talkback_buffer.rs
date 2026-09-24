//! Talkback jitter buffer (reaperiem#154).
//!
//! Absorbs WebSocket arrival jitter from the browser-side Opus encoder.
//! Drain loop pops one frame every 20 ms and sends to the Receive VST
//! via UDP, regardless of push cadence. Overflow drops oldest frame.

#![cfg(feature = "audio")]

use std::collections::VecDeque;

/// Maximum frames we will buffer = target_ms / frame_ms.
/// With 60 ms target and 20 ms frames, capacity = 3 frames.
pub const TARGET_MS: u32 = 60;
pub const FRAME_MS: u32 = 20;

pub struct JitterBuffer {
    buf: VecDeque<(u16, Vec<u8>)>,
    next_seq: u16,
    overflows: u64,
}

impl JitterBuffer {
    pub fn new() -> Self {
        Self {
            buf: VecDeque::with_capacity((TARGET_MS / FRAME_MS) as usize),
            next_seq: 0,
            overflows: 0,
        }
    }

    /// Assign the next sequence number to `frame` and push it.
    /// If buffer is already at capacity, drop the oldest frame and
    /// increment the overflow counter.
    pub fn push(&mut self, frame: Vec<u8>) {
        let cap = (TARGET_MS / FRAME_MS) as usize;
        if self.buf.len() >= cap {
            self.buf.pop_front();
            self.overflows = self.overflows.saturating_add(1);
        }
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.buf.push_back((seq, frame));
    }

    /// Pop the oldest frame, returning (seq, payload).
    pub fn pop(&mut self) -> Option<(u16, Vec<u8>)> {
        self.buf.pop_front()
    }

    /// Current fill in milliseconds.
    pub fn fill_ms(&self) -> u32 {
        (self.buf.len() as u32) * FRAME_MS
    }

    /// Current depth in frames.
    pub fn depth_frames(&self) -> usize {
        self.buf.len()
    }

    /// Total overflow events since buffer was created.
    pub fn overflows(&self) -> u64 {
        self.overflows
    }

    /// Next sequence that will be assigned on push.
    pub fn next_seq(&self) -> u16 {
        self.next_seq
    }
}

impl Default for JitterBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_under_capacity_accumulates() {
        let mut jb = JitterBuffer::new();
        jb.push(vec![1]);
        jb.push(vec![2]);
        assert_eq!(jb.depth_frames(), 2);
        assert_eq!(jb.fill_ms(), 40);
        assert_eq!(jb.overflows(), 0);
    }

    #[test]
    fn push_at_capacity_drops_oldest() {
        let mut jb = JitterBuffer::new();
        jb.push(vec![1]);
        jb.push(vec![2]);
        jb.push(vec![3]); // fills capacity = 3
        jb.push(vec![4]); // overflow, should drop seq 0 (vec![1])
        assert_eq!(jb.depth_frames(), 3);
        assert_eq!(jb.overflows(), 1);
        let (s0, p0) = jb.pop().expect("frame present");
        assert_eq!(s0, 1, "seq 0 was dropped; next pop is seq 1");
        assert_eq!(p0, vec![2]);
    }

    #[test]
    fn pop_empty_returns_none() {
        let mut jb = JitterBuffer::new();
        assert!(jb.pop().is_none());
    }

    #[test]
    fn fifo_order_preserved() {
        let mut jb = JitterBuffer::new();
        jb.push(vec![10]);
        jb.push(vec![20]);
        let (s0, p0) = jb.pop().unwrap();
        let (s1, p1) = jb.pop().unwrap();
        assert_eq!((s0, p0), (0, vec![10]));
        assert_eq!((s1, p1), (1, vec![20]));
    }

    #[test]
    fn seq_is_monotonic_and_wraps() {
        let mut jb = JitterBuffer::new();
        jb.next_seq = u16::MAX;
        jb.push(vec![1]);
        jb.push(vec![2]);
        // Drain to read the seqs we just pushed
        let (s0, _) = jb.pop().unwrap();
        let (s1, _) = jb.pop().unwrap();
        assert_eq!(s0, u16::MAX);
        assert_eq!(s1, 0, "seq must wrap to 0");
    }

    #[test]
    fn fill_ms_matches_depth() {
        let mut jb = JitterBuffer::new();
        assert_eq!(jb.fill_ms(), 0);
        jb.push(vec![1]);
        assert_eq!(jb.fill_ms(), 20);
        jb.push(vec![2]);
        assert_eq!(jb.fill_ms(), 40);
    }

    #[test]
    fn overflow_counter_accumulates() {
        let mut jb = JitterBuffer::new();
        for i in 0..10u16 {
            jb.push(vec![i as u8]);
        }
        // capacity 3 => 10 pushes = 7 overflows
        assert_eq!(jb.overflows(), 7);
        assert_eq!(jb.depth_frames(), 3);
    }

    #[test]
    fn next_seq_getter_reflects_state() {
        let mut jb = JitterBuffer::new();
        assert_eq!(jb.next_seq(), 0, "empty buffer starts at seq 0");
        jb.push(vec![10]);
        assert_eq!(jb.next_seq(), 1, "after 1 push, next_seq is 1");
        jb.push(vec![20]);
        jb.push(vec![30]);
        assert_eq!(jb.next_seq(), 3, "after 3 pushes, next_seq is 3");

        // Force wrap to ensure the getter tracks the underlying field.
        jb.next_seq = u16::MAX;
        assert_eq!(jb.next_seq(), u16::MAX);
        jb.push(vec![40]);
        assert_eq!(jb.next_seq(), 0, "seq wraps via getter too");
    }
}
