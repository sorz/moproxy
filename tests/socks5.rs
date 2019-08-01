#![feature(async_await)]
use tokio::{
    self,
    net::{TcpListener, TcpStream},
    io::{AsyncReadExt, AsyncWriteExt},
};
use moproxy::proxy::socks5::handshake;

#[tokio::test]
async fn test_socks5_domain() {
    let addr = "127.0.0.1:0".parse().unwrap();
    let mut listener = TcpListener::bind(&addr).unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 128];
        stream.read_exact(&mut buf[..3]).await.unwrap();
        assert_eq!(&[5, 1, 0], &buf[..3]);  // ver 5, no auth
        stream.write_all(&[5, 0]).await.unwrap();  // no auth

        stream.read(&mut buf).await.unwrap();
        assert!(buf.starts_with(&[5, 1, 0, 3, 11]));  // domain, len 11
        assert!(buf[5..].starts_with(b"example.com"));
        assert!(buf[16..].starts_with(&[0, 80]));
        stream.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 80]).await.unwrap();

        stream.read(&mut buf).await.unwrap();
        assert!(buf.starts_with(b"early-payload"));        
    });

    let mut stream = TcpStream::connect(&addr).await.unwrap();
    let dest = ("example.com", 80).into();
    let payload = b"early-payload";
    handshake(&mut stream, &dest, Some(payload), false).await.unwrap();

}
