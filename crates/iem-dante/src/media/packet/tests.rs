use super::*;
use crate::wire::tests::Rng;

/// Every (frame, slot) sample of a packet, read back in order.
fn read_all(buf: &[u8], slots: usize, bps: usize) -> Result<Vec<(usize, usize, i32)>, Ragged> {
    let mut got = Vec::new();
    read_frames(buf, slots, bps, |frame, slot, v| got.push((frame, slot, v)))?;
    Ok(got)
}

/// A sample with every bit in use: the low bits a narrow width drops.
fn full(frame: usize, slot: usize) -> i32 {
    (0x1357_9bdf_u32 ^ ((frame as u32) << 20) ^ ((slot as u32).wrapping_mul(0x9e37_79b9))) as i32
}

#[test]
fn round_trips_at_16_24_and_32_bits() {
    for (bps, mask) in [(2, 0xffff_0000_u32), (3, 0xffff_ff00), (4, 0xffff_ffff)] {
        let (slots, frames) = (3, 64);
        let mut out = [0xaau8; 1500];
        let n = write(&mut out, 7, 11, slots, bps, frames, full);
        assert_eq!(n, MEDIA_HEADER + slots * frames * bps, "{bps}");
        assert_eq!(read_header(&out[..n]), Some((7, 11)));
        assert_eq!(read_frames(&out[..n], slots, bps, |_, _, _| {}), Ok(frames));
        let got = read_all(&out[..n], slots, bps).unwrap();
        assert_eq!(got.len(), slots * frames);
        for (i, &(frame, slot, v)) in got.iter().enumerate() {
            assert_eq!((frame, slot), (i / slots, i % slots));
            assert_eq!(
                v as u32,
                full(frame, slot) as u32 & mask,
                "{bps} {frame} {slot}"
            );
        }
        // Nothing past the packet was touched.
        assert!(out[n..].iter().all(|&b| b == 0xaa), "{bps}");
    }
}

#[test]
fn frames_are_interleaved_slot_by_slot() {
    let mut out = [0u8; 64];
    let n = write(&mut out, 0, 0, 2, 3, 2, |frame, slot| {
        (((frame * 2 + slot + 1) as i32) * 0x01_0101) << 8
    });
    assert_eq!(n, 9 + 12);
    assert_eq!(
        &out[9..n],
        &[1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4, 4],
        "frame 0 slot 0, frame 0 slot 1, frame 1 slot 0, frame 1 slot 1"
    );
}

#[test]
fn twenty_four_bit_is_big_endian_and_sign_extended() {
    let mut buf = vec![2, 0, 0, 0, 0, 0, 0, 0, 0];
    buf.extend_from_slice(&[0xff, 0xff, 0xff, 0x80, 0x00, 0x00, 0x7f, 0xff, 0xff]);
    buf.extend_from_slice(&[0x12, 0x34, 0x56, 0xab, 0xcd, 0xef, 0x00, 0x00, 0x01]);
    let got: Vec<i32> = read_all(&buf, 1, 3).unwrap().iter().map(|g| g.2).collect();
    assert_eq!(
        got,
        [
            -256,
            i32::MIN,
            0x7fff_ff00,
            0x1234_5600,
            0xabcd_ef00_u32 as i32,
            0x100
        ]
    );
    // The 24-bit value itself is the sample >> 8, its sign kept.
    let as_24: Vec<i32> = got.iter().map(|v| v >> 8).collect();
    assert_eq!(as_24, [-1, -8_388_608, 8_388_607, 0x12_3456, -0x54_3211, 1]);
    // Written back, the same bytes.
    let mut out = [0u8; 27];
    let n = write(&mut out, 0, 0, 1, 3, 6, |frame, _| got[frame]);
    assert_eq!(&out[..n], &buf[..]);
}

#[test]
fn the_header_is_byte_2_then_seconds_then_samples() {
    let mut out = [0u8; 9];
    assert_eq!(
        write(&mut out, 0x0102_0304, 0x0a0b_0c0d, 4, 3, 0, |_, _| 1),
        9
    );
    assert_eq!(out, [2, 1, 2, 3, 4, 0x0a, 0x0b, 0x0c, 0x0d]);
    assert_eq!(read_header(&out), Some((0x0102_0304, 0x0a0b_0c0d)));
    assert_eq!(read_header(&out[..8]), None);
    assert_eq!(read_header(&[]), None);
    // Byte 0 is not read (inferno reads none of it).
    out[0] = 0x7e;
    assert_eq!(read_header(&out), Some((0x0102_0304, 0x0a0b_0c0d)));
    assert_eq!(read_all(&out, 4, 3), Ok(vec![]));
}

#[test]
fn a_ragged_or_short_packet_is_refused() {
    let mut out = [0u8; 64];
    let n = write(&mut out, 1, 2, 2, 3, 4, full);
    assert_eq!(n, 9 + 24);
    // One byte short, one byte over: not whole frames, nothing read.
    for len in [n - 1, n + 1, 9 + 5, 8, 3, 0] {
        let mut calls = 0;
        assert_eq!(
            read_frames(&out[..len], 2, 3, |_, _, _| calls += 1),
            Err(Ragged),
            "{len}"
        );
        assert_eq!(calls, 0, "{len}");
    }
    // A width or slot count no flow has.
    for (slots, bps) in [(2, 1), (2, 5), (2, 0), (0, 3), (usize::MAX, 4)] {
        assert_eq!(
            read_all(&out[..n], slots, bps),
            Err(Ragged),
            "{slots} {bps}"
        );
    }
    // Writing: too small a buffer or a bad width writes nothing.
    let mut small = [0x55u8; 32];
    assert_eq!(write(&mut small, 1, 2, 2, 3, 4, full), 0);
    assert!(small.iter().all(|&b| b == 0x55));
    for bps in [0, 1, 5] {
        assert_eq!(write(&mut out, 1, 2, 2, bps, 4, full), 0, "{bps}");
    }
    // A size past usize writes nothing either.
    assert_eq!(write(&mut out, 1, 2, usize::MAX, 4, 2, full), 0);
    assert_eq!(write(&mut out, 1, 2, 1, 4, usize::MAX, full), 0);
    // Exactly the buffer's size fits.
    let mut exact = [0u8; 9 + 24];
    assert_eq!(write(&mut exact, 1, 2, 2, 3, 4, full), 33);
}

#[test]
fn random_buffers_never_panic() {
    let mut rng = Rng::new(0x9a7e_7a11);
    let mut out = [0u8; 1500];
    for _ in 0..10_000 {
        let len = rng.below(600);
        let buf: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        let slots = rng.below(20);
        let bps = rng.below(6);
        let _ = read_header(&buf);
        if let Ok(frames) = read_frames(&buf, slots, bps, |_, _, _| {}) {
            assert_eq!(MEDIA_HEADER + frames * slots * bps, len);
        }
        let frames = rng.below(300);
        let cut = rng.below(1501);
        let n = write(&mut out[..cut], 1, 2, slots, bps, frames, |f, s| {
            (f ^ s) as i32
        });
        assert!(n == 0 || n == MEDIA_HEADER + frames * slots * bps);
    }
}
