use bytes::Bytes;
use log::debug;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

#[derive(Debug)]
pub enum Socks5Error {
    ConnectionFailed,
    HandshakeFailed,
    ConnectFailed,
    Timeout,
    IoError(()),
}

pub struct Socks5Client {
    server_addr: SocketAddr,
}

impl Socks5Client {
    pub async fn new(host: IpAddr, port: u16) -> Result<Self, Socks5Error> {
        let server_addr = SocketAddr::new(host, port);
        TcpStream::connect(&server_addr)
            .await
            .map_err(|_| Socks5Error::ConnectionFailed)?;
        debug!("Connection to SOCKS5 server verified at {}:{}", host, port);
        Ok(Socks5Client { server_addr })
    }

    pub async fn forward_request(
        &self,
        hostname: &str,
        port: u16,
        request_data: Bytes,
    ) -> Result<Bytes, Socks5Error> {
        let mut stream = TcpStream::connect(self.server_addr)
            .await
            .map_err(|_| Socks5Error::ConnectionFailed)?;
        debug!("Connected to SOCKS5 server");

        let greeting = [0x05, 0x01, 0x00];
        stream
            .write_all(&greeting)
            .await
            .map_err(|_e| Socks5Error::IoError(()))?;
        debug!("Sent greeting");

        let mut greeting_resp = [0u8; 2];
        stream
            .read_exact(&mut greeting_resp)
            .await
            .map_err(|_e| Socks5Error::IoError(()))?;
        if greeting_resp[0] != 0x05 || greeting_resp[1] != 0x00 {
            return Err(Socks5Error::HandshakeFailed);
        }
        debug!("Greeting accepted");

        let mut connect_req = vec![0x05, 0x01, 0x00, 0x03, hostname.len() as u8];
        connect_req.extend_from_slice(hostname.as_bytes());
        connect_req.extend_from_slice(&port.to_be_bytes());
        stream
            .write_all(&connect_req)
            .await
            .map_err(|_e| Socks5Error::IoError(()))?;
        debug!("Sent CONNECT request for {}:{}", hostname, port);

        let mut resp = [0u8; 10];
        stream
            .read_exact(&mut resp)
            .await
            .map_err(|_e| Socks5Error::IoError(()))?;
        if resp[0] != 0x05 {
            return Err(Socks5Error::HandshakeFailed);
        }
        if resp[1] != 0x00 {
            return Err(Socks5Error::ConnectFailed);
        }
        debug!("CONNECT successful, tunnel established");

        stream
            .write_all(&request_data)
            .await
            .map_err(|_e| Socks5Error::IoError(()))?;
        debug!("Sent HTTP request ({} bytes)", request_data.len());

        let mut response = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match timeout(Duration::from_millis(500), stream.read(&mut buf)).await {
                Ok(Ok(0)) => {
                    debug!("EOF reached");
                    break;
                }
                Ok(Ok(n)) => {
                    response.extend_from_slice(&buf[..n]);
                    debug!("Read {} bytes from tunnel", n);
                }
                Ok(Err(_e)) => {
                    return Err(Socks5Error::IoError(()));
                }
                Err(_) => {
                    debug!("Read timeout - response complete");
                    return Err(Socks5Error::Timeout);  // ← Use Timeout here
                }
            }

        }
        debug!("Received {} bytes total from target", response.len());
        Ok(Bytes::from(response))
    }
}
