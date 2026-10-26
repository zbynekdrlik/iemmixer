use super::*;

use crate::wire::Header;
use crate::wire::tests::Rng;

/// The receiver of the tests: `peer-device`'s flow `flow-1` to
/// 10.0.0.11:4321, TX channels 1 and 2 at 96 kHz, 24 bits, 32 fpp.
fn request() -> FlowRequest {
    FlowRequest {
        rx_host: "peer-device".to_string(),
        rx_flow_name: "flow-1".to_string(),
        sample_rate: 96_000,
        bits: 24,
        channels: vec![Some(1), Some(2)],
        fpp: 32,
        rx: SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 11), 4321),
    }
}

#[test]
fn our_request_is_byte_for_byte_inferno_s() {
    // inferno's FlowsControlClient::request_flow, written out by hand: the
    // strings at 0x26 + 2n + 10 = 52, the flow name at 52 + 12 = 64, one
    // zero to the descriptor at 72 (a multiple of 8), 0x0802, the port, the
    // address; 0x1c + 2n, 0x0a00, 0x0002, fpp, the name's offset and 12
    // zero bytes after the channel ids.
    #[rustfmt::skip]
    let expected: Vec<u8> = vec![
        0x11, 0x02, 0x00, 0x50, 0x00, 0x42, 0x01, 0x00, 0x00, 0x00,
        0x00, 0x34,
        0x00, 0x01, 0x77, 0x00,
        0x00, 0x00, 0x00, 0x18,
        0x00, 0x01,
        0x00, 0x02,
        0x00, 0x48,
        0x00, 0x01, 0x00, 0x02,
        0x00, 0x20,
        0x0a, 0x00,
        0x00, 0x02,
        0x00, 0x20,
        0x00, 0x40,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        b'p', b'e', b'e', b'r', b'-', b'd', b'e', b'v', b'i', b'c', b'e', 0,
        b'f', b'l', b'o', b'w', b'-', b'1', 0,
        0,
        0x08, 0x02, 0x10, 0xe1, 10, 0, 0, 11,
    ];
    let bytes = build_request(0x42, &request());
    assert_eq!(bytes, expected);
    assert_eq!(parse_request(&bytes), Ok(request()));
    let (h, _) = Header::parse(&bytes).unwrap();
    assert_eq!(
        (h.start, h.op1, h.op2, h.seq),
        (START, op::REQUEST, 0, 0x42)
    );
}

#[test]
fn strings_that_end_on_a_multiple_of_8_get_no_padding_and_empty_slots_are_id_0() {
    let req = FlowRequest {
        rx_host: "abcde".to_string(),
        rx_flow_name: "flow-12".to_string(),
        channels: vec![None],
        ..request()
    };
    let bytes = build_request(1, &req);
    // Strings at 10 + 38 + 2 = 50, the name at 56, the descriptor right at
    // 64 with no zero before it.
    assert_eq!(read_u16(&bytes, 10), Some(50));
    assert_eq!(read_u16(&bytes, 10 + 14), Some(64));
    assert_eq!(&bytes[50..56], b"abcde\0");
    assert_eq!(&bytes[56..64], b"flow-12\0");
    assert_eq!(&bytes[64..66], &[0x08, 0x02]);
    assert_eq!(read_u16(&bytes, 10 + 16), Some(0), "the empty slot");
    // 0x1c + 2n after the ids: with one channel 2n and 2 + n differ (with
    // the two of `request()` they do not; PR #79's mutation gate).
    assert_eq!(read_u16(&bytes, 10 + 18), Some(0x1c + 2));
    assert_eq!(bytes.len(), 72);
    assert_eq!(parse_request(&bytes), Ok(req));
}

#[test]
fn many_shapes_parse_back_equal() {
    let mut rng = Rng::new(0x0f10_c0de);
    for n in 0..=16 {
        for name_len in [0, 1, 7, 8, 31] {
            let req = FlowRequest {
                rx_host: "h".repeat(name_len),
                rx_flow_name: "f".repeat(31 - name_len),
                sample_rate: rng.next() as u32,
                bits: rng.next() as u32,
                channels: (0..n)
                    .map(|_| match rng.next() as u16 {
                        0 => None,
                        id => Some(id),
                    })
                    .collect(),
                fpp: rng.next() as u16,
                rx: SocketAddrV4::new(Ipv4Addr::from_bits(rng.next() as u32), rng.next() as u16),
            };
            let bytes = build_request(n as u16, &req);
            let descriptor = usize::from(read_u16(&bytes, 10 + 14).unwrap());
            assert_eq!(descriptor % 8, 0);
            assert_eq!(bytes.len(), descriptor + 8);
            assert_eq!(parse_request(&bytes), Ok(req), "{n} {name_len}");
        }
    }
}

/// `request()`'s datagram with its body cut to `body_len` bytes.
fn cut(body_len: usize) -> Vec<u8> {
    let mut bytes = build_request(7, &request());
    bytes.truncate(HEADER_LEN + body_len);
    bytes
}

#[test]
fn a_body_of_16_bytes_or_less_is_unread() {
    assert_eq!(parse_request(&cut(16)), Err(Unread::Short));
    assert_eq!(parse_request(&cut(0)), Err(Unread::Short));
    assert_eq!(parse_request(&[]), Err(Unread::Short));
    assert_eq!(parse_request(&cut(17)), Err(Unread::Cut));
}

#[test]
fn channel_ids_or_the_fields_after_them_past_the_end_are_unread() {
    // Two ids end the 20th body byte, fpp and the name's offset the 30th:
    // a body of 29 bytes misses the name offset's last byte.
    assert_eq!(parse_request(&cut(29)), Err(Unread::Cut));
    // 30 bytes hold them; the strings' offsets then point past the end.
    assert_eq!(parse_request(&cut(30)), Err(Unread::NoString));
    // A count of ids larger than the datagram holds.
    let mut bytes = build_request(7, &request());
    bytes[10 + 12..10 + 14].copy_from_slice(&40u16.to_be_bytes());
    assert_eq!(parse_request(&bytes), Err(Unread::Cut));
    // 0xffff ids: no overflow, no panic.
    bytes[10 + 12..10 + 14].copy_from_slice(&u16::MAX.to_be_bytes());
    assert_eq!(parse_request(&bytes), Err(Unread::Cut));
}

#[test]
fn a_missing_or_broken_string_is_unread() {
    let base = build_request(7, &request());
    // The device name's offset at the datagram's end, then past it.
    for at in [base.len(), base.len() + 5, usize::from(u16::MAX)] {
        let mut bytes = base.clone();
        bytes[10..12].copy_from_slice(&(at as u16).to_be_bytes());
        assert_eq!(parse_request(&bytes), Err(Unread::NoString), "{at}");
    }
    // The flow name's offset past the end.
    let mut bytes = base.clone();
    bytes[10 + 28..10 + 30].copy_from_slice(&(base.len() as u16).to_be_bytes());
    assert_eq!(parse_request(&bytes), Err(Unread::NoString));
    // Bytes that are not UTF-8.
    let mut bytes = base.clone();
    bytes[52] = 0xff;
    assert_eq!(parse_request(&bytes), Err(Unread::NoString));
    let mut bytes = base;
    bytes[64] = 0xc3;
    assert_eq!(parse_request(&bytes), Err(Unread::NoString));
}

#[test]
fn strings_read_as_inferno_reads_them_to_the_nul_or_the_end_and_inside_the_header() {
    let mut bytes = build_request(7, &request());
    // The device name's offset at 0: inferno reads the header's bytes
    // (11 02, then the length's 00 ends it).
    bytes[10..12].copy_from_slice(&0u16.to_be_bytes());
    assert_eq!(parse_request(&bytes).unwrap().rx_host, "\u{11}\u{2}");
    // A name with no NUL runs to the datagram's end: the flow name's offset
    // at the descriptor's last byte (11, a valid character).
    let mut bytes = build_request(7, &request());
    let last = (bytes.len() - 1) as u16;
    bytes[10 + 28..10 + 30].copy_from_slice(&last.to_be_bytes());
    assert_eq!(parse_request(&bytes).unwrap().rx_flow_name, "\u{b}");
    // An empty name (the offset at a NUL) is a name.
    let mut bytes = build_request(7, &request());
    bytes[10 + 28..10 + 30].copy_from_slice(&63u16.to_be_bytes());
    assert_eq!(parse_request(&bytes).unwrap().rx_flow_name, "");
}

#[test]
fn a_descriptor_past_the_end_is_unread_and_its_tag_is_not_checked() {
    let base = build_request(7, &request());
    let mut bytes = base.clone();
    bytes.truncate(base.len() - 1);
    bytes[2..4].copy_from_slice(&((base.len() - 1) as u16).to_be_bytes());
    assert_eq!(parse_request(&bytes), Err(Unread::NoDescriptor));
    // The descriptor's offset past the end.
    let mut bytes = base.clone();
    bytes[10 + 14..10 + 16].copy_from_slice(&u16::MAX.to_be_bytes());
    assert_eq!(parse_request(&bytes), Err(Unread::NoDescriptor));
    // Exactly 8 bytes from the offset to the end are enough.
    let mut bytes = base.clone();
    bytes[72..74].copy_from_slice(&[0xde, 0xad]);
    assert_eq!(parse_request(&bytes), Ok(request()));
    // The descriptor may lie anywhere, inside the fixed part too: at 32 it
    // reads 0a 00, 00 02, 00 20 00 40.
    let mut bytes = base;
    bytes[10 + 14..10 + 16].copy_from_slice(&32u16.to_be_bytes());
    let parsed = parse_request(&bytes).unwrap();
    assert_eq!(
        parsed.rx,
        SocketAddrV4::new(Ipv4Addr::new(0, 0x20, 0, 0x40), 0x0002)
    );
}

#[test]
fn the_stop_is_exactly_the_handle() {
    let h = handle(5, 0xbeef);
    let bytes = build_stop(9, &h);
    let (header, body) = Header::parse(&bytes).unwrap();
    assert_eq!(
        (header.start, header.seq, header.op1, header.op2),
        (START, 9, op::STOP, 0)
    );
    assert_eq!(body, [0, 0, 0, 5, 0xbe, 0xef]);
    assert_eq!(parse_stop(body), Ok(h));
    assert_eq!(parse_stop(&body[..5]), Err(Unread::NotAHandle));
    assert_eq!(parse_stop(&[body, &[0]].concat()), Err(Unread::NotAHandle));
    assert_eq!(parse_stop(&[]), Err(Unread::NotAHandle));
}

#[test]
fn the_update_carries_the_handle_the_count_and_the_ids() {
    let h = handle(31, 7);
    let channels = [Some(3), None, Some(16)];
    let bytes = build_update(2, &h, &channels);
    let (header, body) = Header::parse(&bytes).unwrap();
    assert_eq!((header.op1, header.op2, header.seq), (op::UPDATE, 0, 2));
    assert_eq!(body, [0, 0, 0, 31, 0, 7, 0, 3, 0, 3, 0, 0, 0, 16]);
    assert_eq!(parse_update(body), Ok((h, channels.to_vec())));
    // Bytes after the ids are ignored, as inferno reads them.
    assert_eq!(
        parse_update(&[body, &[1, 2, 3]].concat()),
        Ok((h, channels.to_vec()))
    );
    // Cut: under 8 bytes, or ids past the end.
    for len in 0..body.len() {
        assert_eq!(parse_update(&body[..len]), Err(Unread::Cut), "{len}");
    }
    // No channels at all.
    assert_eq!(
        parse_update(&[0, 0, 0, 1, 0, 2, 0, 0]),
        Ok((handle(1, 2), vec![]))
    );
}

#[test]
fn a_handle_is_the_slot_then_the_cookie_big_endian() {
    assert_eq!(handle(0x0102_0304, 0x0506), [1, 2, 3, 4, 5, 6]);
    assert_eq!(split(&[1, 2, 3, 4, 5, 6]), (0x0102_0304, 0x0506));
    assert_eq!(split(&handle(u32::MAX, 0)), (u32::MAX, 0));
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// Captured live on 2026-10-26 (#72, Live 2) between our device and a
/// Yamaha Dante card: our stop of the card's earlier grant and its OK, our
/// request for one of its TX channels in four slots at fpp 16 and its
/// grant. P6: our device's name is replaced by one of the same length, our
/// address by 10.0.0.10 and the channel's id by the synthetic map's 94;
/// every other byte is as captured.
#[test]
fn a_real_card_took_these_datagrams_and_its_grant_carries_the_handle() {
    let req = FlowRequest {
        rx_host: "test-device-01".to_string(),
        rx_flow_name: "2_0".to_string(),
        sample_rate: 96_000,
        bits: 24,
        channels: vec![Some(94), None, None, None],
        fpp: 16,
        rx: SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 10), 0xeaad),
    };
    let request = unhex(concat!(
        "1102005800030100000000380001770000000018000100040050005e00000000",
        "000000240a00000200100047000000000000000000000000746573742d646576",
        "6963652d303100325f300000000000000802eaad0a00000a",
    ));
    assert_eq!(build_request(3, &req), request);
    assert_eq!(parse_request(&request), Ok(req));
    // The card's grant: OK, the handle, then 8 bytes we do not read.
    let grant = unhex("11020018000301000001000501d3c7ea0001000100000000");
    let (h, body) = Header::parse(&grant).unwrap();
    assert_eq!((h.start, h.seq, h.op1, h.op2), (START, 3, op::REQUEST, 1));
    assert_eq!(reply_handle(body), Some([0, 5, 1, 0xd3, 0xc7, 0xea]));
    // Our stop of the grant before it, and the card's OK, no body.
    let stop = unhex("11020010000201010000000501d3c7e9");
    assert_eq!(build_stop(2, &[0, 5, 1, 0xd3, 0xc7, 0xe9]), stop);
    let ok = unhex("1102000a000201010001");
    let (h, body) = Header::parse(&ok).unwrap();
    assert_eq!((h.seq, h.op1, h.op2, body.len()), (2, op::STOP, 1, 0));
}

/// A Yamaha Dante card's own request to us, captured live on 2026-10-26
/// (#72, Live 3: its spare return subscribed to our TX 1): our TX 1 in four
/// slots at fpp 32, 24-bit, 96 kHz, to its port 0x3817. P6: the card's name
/// is replaced by one of the same length and its address by 10.0.0.11;
/// every other byte is as captured. We read it, and write it back the same.
#[test]
fn a_real_card_s_request_reads_and_writes_back_byte_for_byte() {
    let request = unhex(concat!(
        "1102005000000100000000380001770000000018000100040048000100000000",
        "000000240a00000200200043000000000000000000000000746573742d636172",
        "6431003900000000080238170a00000b",
    ));
    let want = FlowRequest {
        rx_host: "test-card1".to_string(),
        rx_flow_name: "9".to_string(),
        sample_rate: 96_000,
        bits: 24,
        channels: vec![Some(1), None, None, None],
        fpp: 32,
        rx: SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 11), 0x3817),
    };
    assert_eq!(parse_request(&request), Ok(want.clone()));
    assert_eq!(build_request(0, &want), request);
}

#[test]
fn the_reply_s_handle_is_its_first_6_bytes() {
    assert_eq!(reply_handle(&[1, 2, 3, 4, 5, 6]), Some([1, 2, 3, 4, 5, 6]));
    assert_eq!(
        reply_handle(&[1, 2, 3, 4, 5, 6, 7]),
        Some([1, 2, 3, 4, 5, 6])
    );
    assert_eq!(reply_handle(&[1, 2, 3, 4, 5]), None);
    assert_eq!(reply_handle(&[]), None);
}

#[test]
fn random_bodies_never_panic() {
    let mut rng = Rng::new(0x4455_f10c);
    let base = build_request(1, &request());
    for i in 0..10_000 {
        let len = rng.below(601);
        let mut bytes = if i % 2 == 0 {
            rng.bytes(len)
        } else {
            // A real request with random bytes flipped and cut.
            let mut b = base.clone();
            for _ in 0..1 + rng.below(4) {
                let at = rng.below(b.len());
                b[at] = rng.next() as u8;
            }
            b.truncate(rng.below(b.len() + 1));
            b
        };
        if let Ok(req) = parse_request(&bytes) {
            assert!(bytes.len() > HEADER_LEN + 16);
            assert!(req.channels.len() * 2 + 26 <= bytes.len() - HEADER_LEN);
            assert!(req.channels.iter().all(|c| *c != Some(0)));
        }
        let _ = parse_stop(&bytes);
        let _ = parse_update(&bytes);
        let _ = reply_handle(&bytes);
        bytes.truncate(rng.below(bytes.len() + 1));
        let _ = parse_request(&bytes);
    }
}
