//! Phase 0 go/no-go spike for native ShadowQUIC support in honk.
//!
//! This crate uses exactly the crates honk would add behind a `shadowquic`
//! feature (`quinn-jls` plus the `rustls-jls` it re-exports) and proves, against
//! a real `shadowquic` server, that:
//!
//!   1. a JLS-authenticated QUIC connection can be established at all;
//!   2. `Connection::is_jls()` reports `Some(true)` for correct credentials
//!      (and the authenticated user can be read back);
//!   3. ShadowQUIC request framing — one tag byte plus an RFC 1928 address —
//!      round-trips a real TCP echo through the server (`SQ_REQ_CONNECT`);
//!   4. a second bidi stream works on the same connection;
//!   5. wrong credentials never authenticate.
//!
//! Nothing here reads ShadowQUIC's `Node` config; the wire bytes are built by
//! hand from `shadowquic/src/msgs/squic.rs` so the spike validates the framing
//! understood by honk's future handler, not shadowquic's own code paths.
//!
//! Exit codes: 0 = the requested expectation held, 1 = it did not, 2 = usage.
//!
//! Run through `spike/run-spike.sh`, which starts the server and stub upstream.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use quinn_jls as quinn;
use quinn::crypto::rustls::QuicClientConfig;
use quinn::rustls::jls::JlsClientConfig;
use quinn::rustls::{ClientConfig, RootCertStore};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// `SQ_REQ_CONNECT` — shadowquic/src/msgs/squic.rs:15.
const SQ_REQ_CONNECT: u8 = 0x01;

/// `SQ_REQ_ASSOCIATE_OVER_DATAGRAM` — shadowquic/src/msgs/squic.rs:17.
#[allow(dead_code)]
const SQ_REQ_ASSOCIATE_OVER_DATAGRAM: u8 = 0x03;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const IO_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// Credentials are valid: JLS must authenticate and the echo must work.
    Ok,
    /// Credentials are invalid: the connection must not be JLS-authenticated.
    AuthFail,
}

#[derive(Debug)]
struct Opts {
    server: SocketAddr,
    sni: String,
    user: String,
    password: String,
    alpn: Vec<String>,
    zero_rtt: bool,
    bind: SocketAddr,
    /// Send the target address as ATYP 0x03 (domain) instead of an IP.
    target_domain: Option<String>,
    expect: Expect,
}

impl Opts {
    fn parse() -> Result<Self> {
        let mut opts = Opts {
            server: "127.0.0.1:14430".parse().unwrap(),
            sni: "shadowquic.test".to_string(),
            user: "spike-user".to_string(),
            password: "spike-pass".to_string(),
            alpn: vec!["h3".to_string()],
            zero_rtt: false,
            bind: "0.0.0.0:0".parse().unwrap(),
            target_domain: None,
            expect: Expect::Ok,
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            let mut value = |name: &str| -> Result<String> {
                args.next()
                    .ok_or_else(|| anyhow!("{name} requires a value"))
            };
            match arg.as_str() {
                "--server" => opts.server = value("--server")?.parse()?,
                "--sni" => opts.sni = value("--sni")?,
                "--user" => opts.user = value("--user")?,
                "--password" => opts.password = value("--password")?,
                "--alpn" => {
                    opts.alpn = value("--alpn")?
                        .split(',')
                        .map(|item| item.trim().to_string())
                        .filter(|item| !item.is_empty())
                        .collect();
                }
                "--zero-rtt" => opts.zero_rtt = true,
                "--bind" => opts.bind = value("--bind")?.parse()?,
                "--target-domain" => opts.target_domain = Some(value("--target-domain")?),
                "--expect" => {
                    opts.expect = match value("--expect")?.as_str() {
                        "ok" => Expect::Ok,
                        "authfail" => Expect::AuthFail,
                        other => return Err(anyhow!("--expect must be ok or authfail, got {other}")),
                    }
                }
                "-h" | "--help" => {
                    println!(
                        "usage: shadowquic-jls-spike [--server ADDR] [--sni NAME] [--user U] \
                         [--password P] [--alpn LIST] [--zero-rtt] [--bind ADDR] \
                         [--target-domain NAME] [--expect ok|authfail]"
                    );
                    std::process::exit(2);
                }
                other => return Err(anyhow!("unknown argument {other}")),
            }
        }
        Ok(opts)
    }
}

#[tokio::main]
async fn main() {
    let opts = match Opts::parse() {
        Ok(opts) => opts,
        Err(error) => {
            eprintln!("usage error: {error}");
            std::process::exit(2);
        }
    };
    println!(
        "spike: server={} sni={} alpn={:?} zero_rtt={} expect={:?}",
        opts.server, opts.sni, opts.alpn, opts.zero_rtt, opts.expect
    );
    match run(&opts).await {
        Ok(()) => {
            println!("SPIKE RESULT: PASS (expect={:?})", opts.expect);
            std::process::exit(0);
        }
        Err(error) => {
            println!("SPIKE RESULT: FAIL (expect={:?}): {error:#}", opts.expect);
            std::process::exit(1);
        }
    }
}

async fn run(opts: &Opts) -> Result<()> {
    match opts.expect {
        Expect::Ok => run_ok(opts).await,
        Expect::AuthFail => run_authfail(opts).await,
    }
}

/// Positive case: JLS must authenticate and a TCP echo must round-trip.
async fn run_ok(opts: &Opts) -> Result<()> {
    let echo = spawn_echo().await?;
    println!("spike: local echo target on {echo}");

    let (endpoint, conn) = connect(opts).await?;
    let jls = conn.is_jls();
    println!("spike: handshake complete, is_jls={jls:?}");
    match jls {
        Some(true) => {}
        Some(false) => {
            return Err(anyhow!(
                "JLS authentication failed: server treated the handshake as a probe \
                 (wrong username/password, or the client SNI does not match the server's \
                 jls-upstream host / server-name)"
            ))
        }
        None => return Err(anyhow!("connection is not handshaking, is_jls=None")),
    }
    println!("spike: jls_chosen_user={:?}", conn.jls_chosen_user());

    // Stream 1: IPv4 ATYP.
    let payload = b"shadowquic-jls-spike";
    let got = roundtrip(&conn, echo, None, payload).await?;
    anyhow::ensure!(got == payload, "echo mismatch: sent {payload:?} got {got:?}");
    println!("spike: SQ_CONNECT IPv4 echo ok ({} bytes)", got.len());

    // Stream 2: same connection, proving bidi streams multiplex.
    let payload2 = b"second-stream";
    let got2 = roundtrip(&conn, echo, None, payload2).await?;
    anyhow::ensure!(got2 == payload2, "second stream mismatch: {got2:?}");
    println!("spike: second SQ_CONNECT stream echo ok");

    // Stream 3 (optional): domain ATYP 0x03 instead of an IP address.
    if let Some(domain) = opts.target_domain.as_deref() {
        let got3 = roundtrip(&conn, echo, Some(domain), payload).await?;
        anyhow::ensure!(got3 == payload, "domain ATYP echo mismatch: {got3:?}");
        println!("spike: SQ_CONNECT domain ATYP echo ok (domain={domain})");
    }

    conn.close(0u32.into(), b"spike done");
    endpoint.wait_idle().await;
    Ok(())
}

/// Negative case: bad credentials must not produce a JLS-authenticated
/// connection. Either the handshake fails outright (expected when the server
/// forwards the probe to `jls-upstream` and our empty trust store rejects the
/// relayed certificate) or it completes with `is_jls() != Some(true)`.
async fn run_authfail(opts: &Opts) -> Result<()> {
    let attempt = tokio::time::timeout(Duration::from_secs(8), connect(opts)).await;
    match attempt {
        Err(_) => {
            println!("spike: connect timed out with bad credentials (expected)");
            Ok(())
        }
        Ok(Err(error)) => {
            println!("spike: connect failed with bad credentials (expected): {error:#}");
            Ok(())
        }
        Ok(Ok((endpoint, conn))) => {
            let jls = conn.is_jls();
            println!("spike: connection completed with bad credentials, is_jls={jls:?}");
            let authed = jls == Some(true);
            conn.close(0u32.into(), b"spike done");
            endpoint.wait_idle().await;
            if authed {
                Err(anyhow!(
                    "bad credentials authenticated: the server accepted an invalid user"
                ))
            } else {
                println!("spike: is_jls != Some(true) with bad credentials (expected)");
                Ok(())
            }
        }
    }
}

fn client_config(opts: &Opts) -> Result<quinn::ClientConfig> {
    // Empty trust store on purpose: a JLS-authenticated handshake skips
    // certificate verification (rustls-jls src/client/tls13.rs:1209-1223), and a
    // probe/hijack must fail here rather than succeed silently.
    let roots = RootCertStore::empty();
    let mut crypto = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    crypto.alpn_protocols = opts
        .alpn
        .iter()
        .map(|alpn| alpn.as_bytes().to_vec())
        .collect();
    crypto.enable_early_data = opts.zero_rtt;
    // JlsClientConfig::new(user_pwd, user_iv) — password first, username second.
    crypto.jls_config = JlsClientConfig::new(&opts.password, &opts.user);

    let quic = QuicClientConfig::try_from(crypto).context("build QUIC client crypto")?;
    let mut config = quinn::ClientConfig::new(Arc::new(quic));

    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(Duration::from_secs(5)));
    transport.max_concurrent_bidi_streams(64u32.into());
    config.transport_config(Arc::new(transport));
    Ok(config)
}

async fn connect(opts: &Opts) -> Result<(quinn::Endpoint, quinn::Connection)> {
    let mut endpoint =
        quinn::Endpoint::client(opts.bind).context("bind client UDP endpoint")?;
    endpoint.set_default_client_config(client_config(opts)?);
    let connecting = endpoint
        .connect(opts.server, &opts.sni)
        .context("start QUIC connect")?;
    let conn = tokio::time::timeout(CONNECT_TIMEOUT, async { connecting.await })
        .await
        .map_err(|_| anyhow!("QUIC connect timed out after {CONNECT_TIMEOUT:?}"))?
        .context("QUIC handshake")?;
    Ok((endpoint, conn))
}

/// Open one bidi stream, write `SQ_REQ_CONNECT` + address, send `payload`, and
/// read exactly that many bytes back from the echo target.
async fn roundtrip(
    conn: &quinn::Connection,
    target: SocketAddr,
    domain: Option<&str>,
    payload: &[u8],
) -> Result<Vec<u8>> {
    let mut request = Vec::with_capacity(1 + 1 + 1 + 16 + 2 + domain.map_or(0, str::len));
    request.push(SQ_REQ_CONNECT);
    encode_socks_addr(&mut request, target, domain);

    let (mut send, mut recv) = tokio::time::timeout(IO_TIMEOUT, async { conn.open_bi().await })
        .await
        .map_err(|_| anyhow!("open_bi timed out"))?
        .context("open_bi")?;

    tokio::time::timeout(IO_TIMEOUT, async {
        send.write_all(&request).await?;
        send.write_all(payload).await?;
        let mut got = vec![0u8; payload.len()];
        recv.read_exact(&mut got).await?;
        let _ = send.finish();
        Ok::<Vec<u8>, anyhow::Error>(got)
    })
    .await
    .map_err(|_| anyhow!("stream I/O timed out"))?
}

/// RFC 1928 address encoding: `ATYP | address | port(u16 BE)`.
/// Matches `shadowquic/src/msgs/socks5.rs:26-28`.
fn encode_socks_addr(out: &mut Vec<u8>, addr: SocketAddr, domain: Option<&str>) {
    if let Some(domain) = domain {
        out.push(0x03);
        out.push(domain.len() as u8);
        out.extend_from_slice(domain.as_bytes());
    } else {
        match addr {
            SocketAddr::V4(v4) => {
                out.push(0x01);
                out.extend_from_slice(&v4.ip().octets());
            }
            SocketAddr::V6(v6) => {
                out.push(0x04);
                out.extend_from_slice(&v6.ip().octets());
            }
        }
    }
    out.extend_from_slice(&addr.port().to_be_bytes());
}

/// Local TCP echo server used as the proxy target.
async fn spawn_echo() -> Result<SocketAddr> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind echo listener")?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _peer)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 8192];
                loop {
                    match socket.read(&mut buf).await {
                        Ok(0) => {
                            let _ = socket.shutdown().await;
                            return;
                        }
                        Ok(n) => {
                            if socket.write_all(&buf[..n]).await.is_err() {
                                return;
                            }
                        }
                        Err(_) => return,
                    }
                }
            });
        }
    });
    Ok(addr)
}

#[cfg(test)]
mod tests {
    use super::encode_socks_addr;
    use std::net::SocketAddr;

    #[test]
    fn ipv4_address_encoding() {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let mut out = Vec::new();
        encode_socks_addr(&mut out, addr, None);
        assert_eq!(out.as_slice(), &[0x01u8, 127, 0, 0, 1, 0x1f, 0x90]);
    }

    #[test]
    fn domain_address_encoding() {
        let addr: SocketAddr = "127.0.0.1:443".parse().unwrap();
        let mut out = Vec::new();
        encode_socks_addr(&mut out, addr, Some("example.com"));
        assert_eq!(out[0], 0x03);
        assert_eq!(out[1], 11);
        assert_eq!(&out[2..13], b"example.com");
        assert_eq!(&out[13..15], &[0x01u8, 0xbb]);
    }

    #[test]
    fn ipv6_address_encoding() {
        let addr: SocketAddr = "[::1]:1".parse().unwrap();
        let mut out = Vec::new();
        encode_socks_addr(&mut out, addr, None);
        assert_eq!(out[0], 0x04);
        assert_eq!(
            &out[1..17],
            &[0u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
        );
        assert_eq!(&out[17..19], &[0x00u8, 0x01]);
    }
}
