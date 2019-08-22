use moproxy::proxy::socks5::handshake;
use std::net::SocketAddr;
use tokio::{
    self,
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

#[tokio::test]
async fn test_socks5_domain() {
    let addr = "127.0.0.1:0".parse().unwrap();
    let mut listener = TcpListener::bind(&addr).unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 128];
        stream.read_exact(&mut buf[..3]).await.unwrap();
        assert_eq!(&[5, 1, 0], &buf[..3]); // ver 5, no auth
        stream.write_all(&[5, 0]).await.unwrap(); // no auth

        stream.read(&mut buf).await.unwrap();
        assert!(buf.starts_with(&[5, 1, 0, 3, 11])); // domain, len 11
        assert!(buf[5..].starts_with(b"example.com"));
        assert!(buf[16..].starts_with(&[0, 80]));
        stream
            .write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 80])
            .await
            .unwrap();

        stream.read(&mut buf).await.unwrap();
        assert!(buf.starts_with(b"early-payload"));
        stream.write_all(b"response").await.unwrap();
    });

    let mut stream = TcpStream::connect(&addr).await.unwrap();
    let dest = ("example.com", 80).into();
    let payload = b"early-payload";
    handshake(&mut stream, &dest, Some(payload), false)
        .await
        .unwrap();
    let mut buf = [0u8; 128];
    let n = stream.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"response");
}

#[tokio::test]
async fn test_socks5_ipv6() {
    let addr = "[::1]:0".parse().unwrap();
    let mut listener = TcpListener::bind(&addr).unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 128];
        stream.read_exact(&mut buf[..3]).await.unwrap();
        assert_eq!(&[5, 1, 0], &buf[..3]); // ver 5, no auth
        stream.write_all(&[5, 0]).await.unwrap(); // no auth

        stream.read(&mut buf).await.unwrap();
        assert_eq!(
            &buf[..22],
            &[
                5, 1, 0, 4, 0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                1, // 2001:db8::1
                0, 80, // port number
            ]
        );
        stream
            .write_all(&[
                5, 0, 0, 4, 1, 2, 3, 4, 5, 6, 7, 8, 8, 7, 6, 5, 4, 3, 2, 1, // v6 addr
                0, 80, // port number
            ])
            .await
            .unwrap();

        stream.read(&mut buf).await.unwrap();
        assert!(buf.starts_with(b"early-payload"));
        stream.write_all(b"response").await.unwrap();
    });

    let mut stream = TcpStream::connect(&addr).await.unwrap();
    let dest = "[2001:db8::1]:80".parse::<SocketAddr>().unwrap().into();
    let payload = b"early-payload";
    handshake(&mut stream, &dest, Some(payload), false)
        .await
        .unwrap();
    let mut buf = [0u8; 128];
    let n = stream.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"response");
}
