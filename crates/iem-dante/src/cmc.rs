// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from inferno (https://github.com/teodly/inferno at 9767558):
// inferno_aoip/src/protocol/proto_cmc.rs (the advertisement's layout),
// inferno_aoip/src/device_server/cmc_server.rs (the server) and
// inferno_aoip/src/device_server/settings.rs (the device id from the
// address), Copyright (C) Teo and the inferno contributors,
// GPL-3.0-or-later OR AGPL-3.0-or-later, taken under GPL-3.0-or-later.
// Changes Copyright (C) 2026 iemmixer contributors.

//! CMC on UDP 8800: the device's advertisement (0x1001), which names it by
//! its device id and address. Phase A answers nothing else, as inferno
//! does: any other datagram gets no reply.

use std::io;
use std::net::Ipv4Addr;
use std::sync::Arc;

use tokio::net::UdpSocket;
use tracing::{debug, warn};

use crate::ctl::{Requests, header_op};
use crate::wire::{Header, STATUS_OK, reply};

/// CMC's UDP port.
pub const PORT: u16 = 8800;
/// The advertisement request.
pub const ADVERTISE: u16 = 0x1001;
/// The info request port the advertisement names (inferno's
/// `INFO_REQUEST_PORT`), served by [`crate::info`].
pub const INFO_REQUEST_PORT: u16 = crate::info::REQUEST_PORT;
/// The device's process id (inferno's default): one device on the box.
pub const PROCESS_ID: u16 = 0;
const RECV_BUFFER: usize = 2048;

/// The device's 8-byte id: two zero bytes, the IPv4 address, the process
/// id (inferno's default when none is configured). The mDNS `id` entry is
/// its hex.
pub fn device_id(ip: Ipv4Addr) -> [u8; 8] {
    let [a, b, c, d] = ip.octets();
    let [p, q] = PROCESS_ID.to_be_bytes();
    [0, 0, a, b, c, d, p, q]
}

/// The reply to a CMC datagram: the advertisement for 0x1001 with op2 0,
/// nothing for anything else.
pub fn handle_cmc(req: &[u8], ip: Ipv4Addr) -> Option<Vec<u8>> {
    let (h, _) = Header::parse(req).ok()?;
    if h.op1 != ADVERTISE || h.op2 != 0 {
        debug!(
            op = format_args!("{:#06x}", h.op1),
            seq = h.seq,
            "CMC request not answered"
        );
        return None;
    }
    Some(reply(&h, STATUS_OK, &advertisement(ip)))
}

/// inferno's `DeviceAdvertisement`: process id, device id, 1, 0, the
/// address, the info request port, 0.
fn advertisement(ip: Ipv4Addr) -> Vec<u8> {
    let mut out = Vec::with_capacity(22);
    out.extend_from_slice(&PROCESS_ID.to_be_bytes());
    out.extend_from_slice(&device_id(ip));
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&ip.octets());
    out.extend_from_slice(&INFO_REQUEST_PORT.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out
}

/// The CMC server for the device at `ip` (the address it is reached at,
/// which the advertisement names); every datagram counted in `requests`.
/// Ends only with an error receiving.
pub async fn serve_cmc(sock: UdpSocket, ip: Ipv4Addr, requests: Arc<Requests>) -> io::Result<()> {
    let mut buf = vec![0u8; RECV_BUFFER];
    loop {
        let (n, peer) = sock.recv_from(&mut buf).await?;
        requests.received(PORT, peer.ip(), header_op(&buf[..n]));
        if let Some(bytes) = handle_cmc(&buf[..n], ip)
            && let Err(e) = sock.send_to(&bytes, peer).await
        {
            warn!(error = %e, "a CMC reply could not be sent");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use crate::wire::{Header, STATUS_OK, request};

    /// A synthetic address (public repository, P6).
    const IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 10);

    #[test]
    fn the_device_id_is_the_address_between_zeros() {
        assert_eq!(device_id(IP), [0, 0, 10, 0, 0, 10, 0, 0]);
        assert_eq!(
            device_id(Ipv4Addr::new(192, 0, 2, 254)),
            [0, 0, 192, 0, 2, 254, 0, 0]
        );
    }

    #[test]
    fn the_advertisement_names_the_device_by_its_address() {
        let reply = handle_cmc(&request(0x27ff, 0x4321, ADVERTISE, 0, &[]), IP).unwrap();
        let (h, body) = Header::parse(&reply).unwrap();
        assert_eq!(
            (h.start, h.seq, h.op1, h.op2),
            (0x27ff, 0x4321, ADVERTISE, STATUS_OK)
        );
        #[rustfmt::skip]
        let want = [
            0x00, 0x00,                                     // process id 0
            0x00, 0x00, 10, 0, 0, 10, 0x00, 0x00,           // the device id
            0x00, 0x01, 0x00, 0x00,                         // 1, 0 (inferno)
            10, 0, 0, 10,                                   // the address
            0x21, 0xfc,                                     // the info request port, 8700
            0x00, 0x00,
        ];
        assert_eq!(body, want);
    }

    #[test]
    fn anything_but_the_advertisement_request_gets_no_answer() {
        // inferno answers only 0x1001 with op2 0; Dante Controller asks again.
        for op1 in [0x1000, 0x1002, 0x0000, 0xffff] {
            assert_eq!(
                handle_cmc(&request(0x27ff, 1, op1, 0, &[]), IP),
                None,
                "{op1:#06x}"
            );
        }
        assert_eq!(handle_cmc(&request(0x27ff, 1, ADVERTISE, 1, &[]), IP), None);
        let mut cut = request(0x27ff, 1, ADVERTISE, 0, &[]);
        cut.pop();
        assert_eq!(handle_cmc(&cut, IP), None);
        assert_eq!(handle_cmc(&[], IP), None);
        // A body after the request is no reason to refuse it.
        assert!(handle_cmc(&request(0x27ff, 1, ADVERTISE, 0, &[1, 2, 3]), IP).is_some());
    }

    #[tokio::test]
    async fn the_cmc_server_answers_the_advertisement_request() {
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = sock.local_addr().unwrap();
        let requests = Arc::new(Requests::default());
        let task = tokio::spawn(serve_cmc(sock, IP, Arc::clone(&requests)));
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        // An unknown request gets no answer; the next one does.
        peer.send_to(&request(0x27ff, 1, 0x1000, 0, &[]), addr)
            .await
            .unwrap();
        peer.send_to(&request(0x27ff, 2, ADVERTISE, 0, &[]), addr)
            .await
            .unwrap();
        let mut buf = [0u8; 256];
        let (n, _) = tokio::time::timeout(Duration::from_secs(5), peer.recv_from(&mut buf))
            .await
            .expect("an answer within 5 s")
            .unwrap();
        let (h, body) = Header::parse(&buf[..n]).unwrap();
        assert_eq!(
            (h.seq, h.op1, h.op2, body.len()),
            (2, ADVERTISE, STATUS_OK, 22)
        );
        assert_eq!(body[14..18], [10, 0, 0, 10]);
        // Both datagrams counted for the status, by port, peer and opcode.
        let counts = requests.counts();
        let seen: Vec<(u16, usize, u16, u64)> = counts
            .received
            .iter()
            .map(|r| (r.port, r.peer, r.op, r.n))
            .collect();
        assert_eq!(seen, [(PORT, 1, 0x1000, 1), (PORT, 1, ADVERTISE, 1)]);
        task.abort();
    }
}
