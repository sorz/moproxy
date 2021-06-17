pub mod iface;
use pnet_packet::{ipv4::Ipv4Packet, ipv6::Ipv6Packet, udp::UdpPacket, Packet};
use std::{
    mem,
    net::{SocketAddrV4, SocketAddrV6},
};

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub enum SocketPair {
    V4(SocketPairV4),
    V6(SocketPairV6),
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct SocketPairV4 {
    src: SocketAddrV4,
    dst: SocketAddrV4,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct SocketPairV6 {
    src: SocketAddrV6,
    dst: SocketAddrV6,
}

#[derive(Debug, Clone)]
pub struct UdpData {
    pub sockets: SocketPair,
    pub data: Box<[u8]>,
}

impl From<SocketPairV6> for SocketPair {
    fn from(pair: SocketPairV6) -> Self {
        Self::V6(pair)
    }
}

impl From<SocketPairV4> for SocketPair {
    fn from(pair: SocketPairV4) -> Self {
        Self::V4(pair)
    }
}

impl From<(&Ipv6Packet<'_>, &UdpPacket<'_>)> for UdpData {
    fn from(pkts: (&Ipv6Packet<'_>, &UdpPacket<'_>)) -> Self {
        let (ip, udp) = pkts;
        let sockets = SocketPairV6 {
            src: SocketAddrV6::new(ip.get_source(), udp.get_source(), 0, 0),
            dst: SocketAddrV6::new(ip.get_destination(), udp.get_destination(), 0, 0),
        }
        .into();
        let data = udp.payload().into();
        Self { sockets, data }
    }
}

impl From<(&Ipv4Packet<'_>, &UdpPacket<'_>)> for UdpData {
    fn from(pkts: (&Ipv4Packet<'_>, &UdpPacket<'_>)) -> Self {
        let (ip, udp) = pkts;
        let sockets = SocketPairV4 {
            src: SocketAddrV4::new(ip.get_source(), udp.get_source()),
            dst: SocketAddrV4::new(ip.get_destination(), udp.get_destination()),
        }
        .into();
        let data = udp.payload().into();
        Self { sockets, data }
    }
}

impl SocketPair {
    pub fn reverse(&mut self) {
        match self {
            SocketPair::V4(socks) => mem::swap(&mut socks.src, &mut socks.dst),
            SocketPair::V6(socks) => mem::swap(&mut socks.src, &mut socks.dst),
        }
    }
}
