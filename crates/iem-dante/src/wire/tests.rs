use super::*;

/// SplitMix64: a seeded generator for the fuzz-style tests (no dependency).
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A value in `0..n` (`n > 0`).
    pub(crate) fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    pub(crate) fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }
}

#[test]
fn a_header_round_trips() {
    let body = [0xde, 0xad, 0xbe, 0xef, 0x00];
    let header = Header {
        start: 0x27ff,
        len: (HEADER_LEN + body.len()) as u16,
        seq: 0x1234,
        op1: 0x3000,
        op2: 0x0000,
    };
    let mut packet = header.to_bytes().to_vec();
    assert_eq!(
        packet,
        [0x27, 0xff, 0x00, 0x0f, 0x12, 0x34, 0x30, 0x00, 0x00, 0x00]
    );
    packet.extend_from_slice(&body);
    let (parsed, rest) = Header::parse(&packet).unwrap();
    assert_eq!(parsed, header);
    assert_eq!(rest, body);
    // Every field is big-endian and in its own place.
    let header = Header {
        start: 0x0102,
        len: 10,
        seq: 0x0506,
        op1: 0x0708,
        op2: 0x090a,
    };
    let bytes = header.to_bytes();
    assert_eq!(bytes, [1, 2, 0, 10, 5, 6, 7, 8, 9, 10]);
    assert_eq!(Header::parse(&bytes).unwrap(), (header, &[][..]));
}

#[test]
fn a_short_datagram_is_an_error() {
    let full = request(0x27ff, 1, 0x1000, 0, &[]);
    for n in 0..HEADER_LEN {
        assert_eq!(Header::parse(&full[..n]), Err(WireError::Short(n)));
    }
    assert!(Header::parse(&full).is_ok());
}

#[test]
fn a_length_that_disagrees_is_an_error() {
    let packet = request(0x27ff, 7, 0x2000, 0, &[0, 1, 0, 1, 0, 0]);
    assert_eq!(packet[2..4], [0x00, 0x10]);
    // Cut short, padded, or a length field that lies either way.
    assert_eq!(
        Header::parse(&packet[..15]),
        Err(WireError::Length {
            header: 16,
            actual: 15
        })
    );
    let mut padded = packet.clone();
    padded.push(0);
    assert_eq!(
        Header::parse(&padded),
        Err(WireError::Length {
            header: 16,
            actual: 17
        })
    );
    for lie in [0u16, 9, 10, 15, 17, 0xffff] {
        let mut bad = packet.clone();
        bad[2..4].copy_from_slice(&lie.to_be_bytes());
        assert_eq!(
            Header::parse(&bad),
            Err(WireError::Length {
                header: lie,
                actual: 16
            }),
            "{lie}"
        );
    }
}

#[test]
fn a_reply_keeps_the_request_sequence() {
    let req = request(0x2729, 0xbeef, 0x1002, 0, &[]);
    let (header, _) = Header::parse(&req).unwrap();
    let out = reply(&header, STATUS_OK, b"name\0");
    let (back, body) = Header::parse(&out).unwrap();
    assert_eq!(
        back,
        Header {
            start: 0x2729,
            len: 15,
            seq: 0xbeef,
            op1: 0x1002,
            op2: STATUS_OK,
        }
    );
    assert_eq!(body, b"name\0");
    // The status rides in op2: more pages, or an error with no body.
    let more = reply(&header, STATUS_MORE, &[1, 2]);
    assert_eq!(
        more[..10],
        [0x27, 0x29, 0x00, 0x0c, 0xbe, 0xef, 0x10, 0x02, 0x81, 0x12]
    );
    let error = reply(&header, STATUS_ERROR, &[]);
    assert_eq!(
        error,
        [0x27, 0x29, 0x00, 0x0a, 0xbe, 0xef, 0x10, 0x02, 0xff, 0xff]
    );
}

#[test]
fn a_request_is_built_as_netaudio_sends_it() {
    // netaudio's channel count request: 27ff 000a <seq> 1000 0000.
    assert_eq!(
        request(0x27ff, 0x1234, 0x1000, 0, &[]),
        [0x27, 0xff, 0x00, 0x0a, 0x12, 0x34, 0x10, 0x00, 0x00, 0x00]
    );
    // Its first page of RX channels: 27ff 0010 <seq> 3000 0000 0001 0001 0000.
    assert_eq!(
        request(0x27ff, 0x0001, 0x3000, 0, &[0, 1, 0, 1, 0, 0]),
        [
            0x27, 0xff, 0x00, 0x10, 0x00, 0x01, 0x30, 0x00, 0x00, 0x00, 0, 1, 0, 1, 0, 0
        ]
    );
}

#[test]
fn random_datagrams_never_panic() {
    let mut rng = Rng::new(0x5eed_d47e);
    let mut parsed = 0;
    for i in 0..10_000 {
        let len = rng.below(601);
        let mut buf = rng.bytes(len);
        // Every other buffer of header size gets a true length field, so the
        // accepting path is walked as often as the refusing one.
        if i % 2 == 0 && len >= HEADER_LEN {
            buf[2..4].copy_from_slice(&(len as u16).to_be_bytes());
        }
        match Header::parse(&buf) {
            Ok((header, body)) => {
                parsed += 1;
                assert_eq!(usize::from(header.len), len);
                assert_eq!(body, &buf[HEADER_LEN..]);
                assert_eq!(header.to_bytes(), buf[..HEADER_LEN]);
                // Reading strings at any offset never panics either.
                for _ in 0..4 {
                    let offset = rng.next() as u16;
                    if let Some(s) = string_at(body, offset) {
                        assert!(!s.contains('\0'));
                        assert!(usize::from(offset) >= HEADER_LEN);
                    }
                }
            }
            Err(WireError::Short(n)) => assert!(n < HEADER_LEN && n == len),
            Err(WireError::Length { header, actual }) => {
                assert_eq!(actual, len);
                assert_ne!(usize::from(header), len);
            }
        }
    }
    assert!(parsed > 4_000, "only {parsed} of 10 000 parsed");
}

#[test]
fn a_string_is_read_at_its_packet_offset() {
    // Offsets count from the packet's first byte, the header included.
    let body = b"\x00\x01abc\0de\0";
    assert_eq!(string_at(body, 12), Some("abc"));
    assert_eq!(string_at(body, 13), Some("bc"));
    assert_eq!(string_at(body, 15), Some(""));
    assert_eq!(string_at(body, 16), Some("de"));
    assert_eq!(string_at(body, 18), Some(""));
    // Without its terminator a string runs to the end, as inferno reads it.
    assert_eq!(string_at(b"\x00\x01xyz", 12), Some("xyz"));
    // An offset inside the header or past the end, or bytes that are not UTF-8.
    assert_eq!(string_at(body, 0), None);
    assert_eq!(string_at(body, 9), None);
    assert_eq!(string_at(body, 10), Some(""));
    assert_eq!(string_at(body, 19), None);
    assert_eq!(string_at(body, 20), None);
    assert_eq!(string_at(b"\xff\xfe\0", 10), None);
}

#[test]
fn a_u16_is_read_big_endian_or_not_at_all() {
    let body = [0x12, 0x34, 0x56];
    assert_eq!(read_u16(&body, 0), Some(0x1234));
    assert_eq!(read_u16(&body, 1), Some(0x3456));
    assert_eq!(read_u16(&body, 2), None);
    assert_eq!(read_u16(&body, usize::MAX), None);
}

#[test]
fn a_body_writer_hands_out_packet_offsets() {
    let mut body = Body::new();
    assert_eq!(body.offset(), 10);
    body.u8(7);
    body.u16(0x0102);
    body.u32(0x0304_0506);
    assert_eq!(body.offset(), 17);
    let at = body.cstr("ab");
    assert_eq!(at, 17);
    body.zeros(2);
    body.set_u8(10, 9);
    body.set_u16(11, 0xa0b0);
    body.set_bytes(14, &[0xc1, 0xc2, 0xc3]);
    assert_eq!(body.offset(), 22);
    let bytes = body.into_vec();
    assert_eq!(
        bytes,
        [9, 0xa0, 0xb0, 3, 0xc1, 0xc2, 0xc3, b'a', b'b', 0, 0, 0]
    );
    assert_eq!(string_at(&bytes, at), Some("ab"));
}

#[test]
fn a_body_aligns_on_packet_offsets_the_header_counted() {
    let mut body = Body::new();
    // Packet offset 10: two zeros up to 12.
    body.align(4);
    assert_eq!(body.offset(), 12);
    // Aligned already: nothing added.
    body.align(4);
    assert_eq!(body.offset(), 12);
    body.bytes(&[1, 2, 3]);
    body.align(4);
    assert_eq!(body.offset(), 16);
    body.u8(4);
    body.align(8);
    assert_eq!(body.offset(), 24);
    assert_eq!(body.into_vec(), [0, 0, 1, 2, 3, 0, 4, 0, 0, 0, 0, 0, 0, 0]);
}
