use super::SocketPair;
use super::UdpData;
use crate::linux::tun::Tun;
use bytes::BytesMut;
use log;
use pnet_packet::{
    ip::IpNextHeaderProtocols,
    ipv4::{self, Ipv4Packet, MutableIpv4Packet},
    ipv6::{Ipv6Packet, MutableIpv6Packet},
    udp::{self, MutableUdpPacket, UdpPacket},
    Packet,
};
use std::{
    convert::TryInto,
    io::{self, Write},
};

const BUF_SIZE: usize = 2048;

pub struct UdpIface {
    tun: Tun,
}

impl UdpIface {
    pub fn new<S: AsRef<str>>(name: S) -> Self {
        let tun = Tun::new(name.as_ref()).expect("Failed to create tun device");
        UdpIface { tun }
    }

    pub async fn read_packet(&self) -> io::Result<UdpData> {
        let mut buf = BytesMut::with_capacity(1500);
        loop {
            buf.resize(buf.capacity(), 0);
            let n = self.tun.read(&mut buf).await?;
            buf.truncate(n);
            match parse_udp_packet(&buf) {
                None => continue,
                Some(udp) => break Ok(udp),
            }
        }
    }

    pub async fn write_packet(&self, pkt: &UdpData) -> io::Result<()> {
        // Build UDP packet header
        let mut udp_buf = [0u8; UdpPacket::minimum_packet_size()];
        let mut udp_pkt = MutableUdpPacket::new(&mut udp_buf).unwrap();
        let (src, dst) = match &pkt.sockets {
            SocketPair::V6(socks) => (socks.src.port(), socks.dst.port()),
            SocketPair::V4(socks) => (socks.src.port(), socks.dst.port()),
        };
        let udp_size = pkt.data.len() + UdpPacket::minimum_packet_size();
        udp_pkt.set_source(src);
        udp_pkt.set_destination(dst);
        udp_pkt.set_length(udp_size.try_into().unwrap());
        let udp_checksum = match &pkt.sockets {
            SocketPair::V6(socks) => {
                udp::ipv6_checksum(&udp_pkt.to_immutable(), &socks.src.ip(), &socks.dst.ip())
            }
            SocketPair::V4(socks) => {
                udp::ipv4_checksum(&udp_pkt.to_immutable(), &socks.src.ip(), &socks.dst.ip())
            }
        };
        udp_pkt.set_checksum(udp_checksum);

        // Build IP packet header
        let mut ip_buf = [0u8; MutableIpv6Packet::minimum_packet_size()];
        let ip_size = match &pkt.sockets {
            SocketPair::V6(socks) => {
                let mut ip_pkt = MutableIpv6Packet::new(&mut ip_buf).unwrap();
                ip_pkt.set_version(6);
                ip_pkt.set_payload_length(udp_size.try_into().unwrap());
                ip_pkt.set_next_header(IpNextHeaderProtocols::Udp);
                ip_pkt.set_hop_limit(64);
                ip_pkt.set_source(*socks.src.ip());
                ip_pkt.set_destination(*socks.dst.ip());
                MutableIpv6Packet::minimum_packet_size() + udp_size
            }
            SocketPair::V4(socks) => {
                let ip_size = MutableIpv4Packet::minimum_packet_size() + udp_size;
                let mut ip_pkt = MutableIpv4Packet::new(&mut ip_buf).unwrap();
                ip_pkt.set_version(4);
                ip_pkt.set_header_length(5); // minimum 20 bytes
                ip_pkt.set_total_length(ip_size.try_into().unwrap());
                ip_pkt.set_ttl(64);
                ip_pkt.set_source(*socks.src.ip());
                ip_pkt.set_destination(*socks.dst.ip());
                ip_pkt.set_checksum(ipv4::checksum(&ip_pkt.to_immutable()));
                ip_size
            }
        };
        if ip_size > BUF_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pakcet too large",
            ));
        }

        // Concat headers & payload
        let mut buf = [0u8; BUF_SIZE];
        let size = buf.len() - {
            let mut pos = &mut buf[..];
            pos.write(&ip_buf)?;
            pos.write(&udp_buf)?;
            pos.write(&pkt.data)?;
            pos.len()
        };
        self.tun.write(&buf[..size]).await
    }
}

impl From<Tun> for UdpIface {
    fn from(tun: Tun) -> Self {
        UdpIface { tun }
    }
}

fn parse_udp_packet(buf: &[u8]) -> Option<UdpData> {
    match buf.first()? >> 4 {
        6 => {
            let ip_pkt = Ipv6Packet::new(&buf)?;
            if ip_pkt.get_next_header() != IpNextHeaderProtocols::Udp {
                return None;
            }
            let udp_pkt = UdpPacket::new(ip_pkt.payload())?;
            Some((&ip_pkt, &udp_pkt).into())
        }
        4 => {
            let ip_pkt = Ipv4Packet::new(&buf)?;
            if ip_pkt.get_next_level_protocol() != IpNextHeaderProtocols::Udp {
                return None;
            }
            let udp_pkt = UdpPacket::new(ip_pkt.payload())?;
            Some((&ip_pkt, &udp_pkt).into())
        }
        _ => None,
    }
}
