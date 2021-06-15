use super::UdpData;
use crate::linux::tun::Tun;
use bytes::BytesMut;
use log;
use pnet_packet::{ip::IpNextHeaderProtocols, ipv6::Ipv6Packet, udp::UdpPacket, Packet};
use std::io;

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
}

impl From<Tun> for UdpIface {
    fn from(tun: Tun) -> Self {
        UdpIface { tun }
    }
}

fn parse_udp_packet(buf: &[u8]) -> Option<UdpData> {
    let ip_pkt = Ipv6Packet::new(&buf)?;
    if ip_pkt.get_next_header() != IpNextHeaderProtocols::Udp {
        log::debug!("Drop non-UDP packet");
        return None;
    }
    let udp_pkt = UdpPacket::new(ip_pkt.payload())?;
    Some((&ip_pkt, &udp_pkt).into())
}
