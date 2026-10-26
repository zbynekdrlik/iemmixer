// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from inferno (https://github.com/teodly/inferno at 9767558):
// inferno_aoip/src/device_server/info_mcast_server.rs (the requests it
// answers, the replies' bodies, the heartbeat, the groups and ports) and
// inferno_aoip/src/protocol/mcast.rs (the 32-byte header), Copyright (C)
// Teo and the inferno contributors, GPL-3.0-or-later OR AGPL-3.0-or-later,
// taken under GPL-3.0-or-later. Changes Copyright (C) 2026 iemmixer
// contributors.

//! Dante's info requests and heartbeat (#72 phase A).

use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

use tokio::net::UdpSocket;

use crate::device::DeviceConfig;

/// The info request port (UDP), at the device's address.
pub const REQUEST_PORT: u16 = 8700;
/// The group and port the info replies go to.
pub const INFO_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 231);
pub const INFO_PORT: u16 = 8702;
/// The group and port the heartbeat goes to.
pub const HEARTBEAT_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 233);
pub const HEARTBEAT_PORT: u16 = 8708;
/// The heartbeat's period.
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(1);
/// The header's size.
pub const HEADER_LEN: usize = 32;

/// The interface's facts the network info carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetInfo {
    pub mac: [u8; 6],
    pub ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
    pub gateway: Ipv4Addr,
    pub link_speed: u16,
}

/// What the info messages say about the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub device_id: [u8; 8],
    pub manufacturer: String,
    pub model: String,
    pub version: [u8; 4],
    pub net: NetInfo,
}

impl Identity {
    pub fn new(_cfg: &DeviceConfig, _net: NetInfo) -> Self {
        todo!()
    }
}

/// What a request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    BoardInfo,
    ProductInfo,
    NetworkInfo,
    Op0077,
    ClockStats,
    Unknown,
}

/// A request received on [`REQUEST_PORT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    pub seq: u16,
    pub opcode: [u8; 8],
    pub kind: Kind,
}

pub fn parse_request(_datagram: &[u8]) -> Option<Request> {
    todo!()
}

/// The device's sender of info packets.
#[derive(Debug, Clone)]
pub struct Multicaster {
    id: Identity,
    seq: u16,
}

impl Multicaster {
    pub fn new(id: Identity) -> Self {
        Self { id, seq: 0 }
    }

    pub fn board_info(&mut self) -> Vec<u8> {
        todo!()
    }

    pub fn product_info(&mut self) -> Vec<u8> {
        todo!()
    }

    pub fn heartbeat(&mut self) -> Vec<u8> {
        todo!()
    }

    pub fn answer(&mut self, _datagram: &[u8]) -> Option<Vec<u8>> {
        todo!()
    }
}

/// Where the info packets go, and how often the heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Multicast {
    pub info: SocketAddrV4,
    pub heartbeat: SocketAddrV4,
    pub every: Duration,
}

impl Multicast {
    pub const DANTE: Multicast = Multicast {
        info: SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0),
        heartbeat: SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0),
        every: Duration::ZERO,
    };
}

pub async fn serve_info(_sock: UdpSocket, _mc: Multicaster, _to: Multicast) -> io::Result<()> {
    todo!()
}

#[cfg(test)]
mod tests;
