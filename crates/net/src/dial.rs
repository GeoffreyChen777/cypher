//! Happy-eyeballs WebSocket dialing (RFC 8305, simplified).
//!
//! `tokio_tungstenite::connect_async` tries resolved addresses SEQUENTIALLY
//! with no per-address bound, so a network that advertises IPv6 but blackholes
//! it (captive portals, airplane wifi) hangs the first SYN until the caller's
//! whole-dial timeout — and the retry repeats the identical order, so the
//! socket never comes up at all. Browsers (and this codebase's reqwest stack
//! via hyper-util) race address families and connect fine on the same network;
//! every WebSocket dial goes through here so they behave the same way:
//! resolve, interleave v6/v4, start one TCP attempt every [`STAGGER`] (or
//! immediately when the previous attempt fails), first connected stream wins
//! and the losers are dropped mid-flight.
//!
//! The winner also gets `TCP_NODELAY` — the protocols upstairs exchange
//! back-to-back small frames (join → eph-join, ack → push) that Nagle would
//! pointlessly pair with delayed ACKs.
//!
//! Proxies, for the same reason: reqwest honours `HTTPS_PROXY` / `HTTP_PROXY`
//! / `ALL_PROXY` and `NO_PROXY` from the environment, and this dialer used to
//! connect straight to the target regardless. Behind a network whose only way
//! out is an HTTP proxy, every HTTPS request worked and every WebSocket failed
//! before reaching the Edge -- so the client silently degraded to HTTP polling
//! every 30s, which is how it was found: one host, 142 HTTP requests in 30
//! minutes and not a single socket attempt. A WebSocket dial now reads the
//! same variables and tunnels through the proxy with HTTP `CONNECT`.

use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::stream::{FuturesUnordered, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::error::{Error as WsError, UrlError};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// Delay before starting the next address attempt while one is still pending
/// (RFC 8305 §5's "Connection Attempt Delay"; 250ms is its recommended value).
const STAGGER: Duration = Duration::from_millis(250);

pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Dial `url` (ws/wss) with happy-eyeballs TCP racing, then run TLS + the
/// WebSocket handshake on the winning stream. A success also broadcasts
/// [`crate::wake::notify_online`] so sibling sockets waiting out a reconnect
/// backoff redial immediately instead of sleeping through the recovery.
// `WsError` is tungstenite's own error enum: its size is not ours to change,
// and boxing it here would force every caller to unwrap an extra indirection
// on a path that runs once per socket dial.
#[allow(clippy::result_large_err)]
pub async fn connect_ws(url: &str) -> Result<WsStream, WsError> {
    let request = url.into_client_request()?;
    connect_request(request).await
}

#[allow(clippy::result_large_err)] // tungstenite's error type — see above.
pub async fn connect_request(
    request: tokio_tungstenite::tungstenite::http::Request<()>,
) -> Result<WsStream, WsError> {
    let uri = request.uri();
    let host = uri
        .host()
        .ok_or(WsError::Url(UrlError::NoHostName))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    let port = uri.port_u16().unwrap_or(match uri.scheme_str() {
        Some("wss") => 443,
        _ => 80,
    });
    let proxy = proxy_for(uri.scheme_str(), &host, port, |name| {
        std::env::var(name).ok()
    });
    let stream = match proxy {
        Some(proxy) => match tunnel(&proxy, &host, port).await {
            Ok(stream) => {
                if !PROXY_USED.swap(true, Ordering::Relaxed) {
                    tracing::info!(proxy = %proxy.endpoint(), "WebSocket dials go through the configured proxy");
                }
                stream
            }
            Err(err) => {
                // Never worse than before: a proxy that cannot tunnel still
                // leaves the direct route, which is all the dialer ever had.
                if !PROXY_FAILED.swap(true, Ordering::Relaxed) {
                    tracing::warn!(proxy = %proxy.endpoint(), error = %err,
                        "WebSocket proxy tunnel failed; trying a direct connection");
                } else {
                    tracing::debug!(proxy = %proxy.endpoint(), error = %err,
                        "WebSocket proxy tunnel failed; trying a direct connection");
                }
                direct(&host, port).await?
            }
        },
        None => direct(&host, port).await?,
    };
    // Best-effort: a socket that works without NODELAY beats no socket.
    let _ = stream.set_nodelay(true);
    let (ws, _response) =
        tokio_tungstenite::client_async_tls_with_config(request, stream, None, None).await?;
    crate::wake::notify_online();
    Ok(ws)
}

/// First proxy use and first proxy failure are worth a log line each; every
/// later dial repeats the same story at debug level.
static PROXY_USED: AtomicBool = AtomicBool::new(false);
static PROXY_FAILED: AtomicBool = AtomicBool::new(false);

/// Resolve and race a direct TCP connection to `host:port`.
async fn direct(host: &str, port: u16) -> io::Result<TcpStream> {
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port)).await?.collect();
    race_connect(interleave_families(addrs))
        .await
        .map_err(|err| io::Error::new(err.kind(), format!("{host}:{port}: {err}")))
}

/// An HTTP proxy the environment routes a dial through.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Proxy {
    host: String,
    port: u16,
    /// `Basic ...` value for `Proxy-Authorization`, when the URL carried
    /// credentials. Never logged: [`Proxy::endpoint`] is what logs show.
    authorization: Option<String>,
}

impl Proxy {
    fn endpoint(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// The proxy a dial to `scheme://host:port` should use, read through `get`
/// (the environment in production). The same variables as reqwest, so both
/// transports reach the network the same way: `HTTPS_PROXY` for `wss`,
/// `HTTP_PROXY` for `ws`, then `ALL_PROXY`; upper case before lower; and
/// `NO_PROXY` excludes. Loopback is never proxied -- a proxy cannot reach this
/// machine's own services, and a local development Edge lives there.
fn proxy_for(
    scheme: Option<&str>,
    host: &str,
    port: u16,
    get: impl Fn(&str) -> Option<String>,
) -> Option<Proxy> {
    if is_loopback(host) {
        return None;
    }
    let var = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| get(name).filter(|value| !value.trim().is_empty()))
    };
    if var(&["NO_PROXY", "no_proxy"]).is_some_and(|list| no_proxy_matches(&list, host, port)) {
        return None;
    }
    let raw = match scheme {
        Some("wss") | Some("https") => var(&["HTTPS_PROXY", "https_proxy"]),
        _ => var(&["HTTP_PROXY", "http_proxy"]),
    }
    .or_else(|| var(&["ALL_PROXY", "all_proxy"]))?;
    let proxy = parse_proxy(&raw);
    if proxy.is_none() {
        tracing::debug!("proxy variable names an unsupported proxy; dialing directly");
    }
    proxy
}

fn is_loopback(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// `NO_PROXY`: comma-separated; `*` matches everything; a domain matches
/// itself and its subdomains (a leading dot is optional); an IP or CIDR block
/// matches addresses; an entry may pin a port with `:port`.
fn no_proxy_matches(list: &str, host: &str, port: u16) -> bool {
    let host = host.to_ascii_lowercase();
    let ip = host.parse::<std::net::IpAddr>().ok();
    list.split(',')
        .map(|entry| entry.trim().to_ascii_lowercase())
        .filter(|entry| !entry.is_empty())
        .any(|entry| {
            if entry == "*" {
                return true;
            }
            // Split an optional port, leaving bare IPv6 literals intact.
            let (name, entry_port) = match entry.rsplit_once(':') {
                Some((name, p)) if !name.contains(':') || name.ends_with(']') => (
                    name.trim_matches(['[', ']']).to_string(),
                    p.parse::<u16>().ok(),
                ),
                _ => (entry.trim_matches(['[', ']']).to_string(), None),
            };
            if entry_port.is_some_and(|p| p != port) {
                return false;
            }
            if let Some(ip) = ip {
                if let Ok(entry_ip) = name.parse::<std::net::IpAddr>() {
                    return entry_ip == ip;
                }
                if let Some((net, bits)) = name.split_once('/') {
                    return cidr_contains(net, bits, ip);
                }
                return false;
            }
            let domain = name.trim_start_matches('.');
            !domain.is_empty() && (host == domain || host.ends_with(&format!(".{domain}")))
        })
}

fn cidr_contains(net: &str, bits: &str, ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    let (Ok(net), Ok(bits)) = (net.parse::<IpAddr>(), bits.parse::<u32>()) else {
        return false;
    };
    match (net, ip) {
        (IpAddr::V4(net), IpAddr::V4(ip)) if bits <= 32 => {
            let mask = u32::MAX.checked_shl(32 - bits).unwrap_or(0);
            u32::from(net) & mask == u32::from(ip) & mask
        }
        (IpAddr::V6(net), IpAddr::V6(ip)) if bits <= 128 => {
            let mask = u128::MAX.checked_shl(128 - bits).unwrap_or(0);
            u128::from(net) & mask == u128::from(ip) & mask
        }
        _ => false,
    }
}

/// `[http://][user:pass@]host[:port][/]`. Only plain-HTTP proxies can be
/// tunnelled here; `https://` and `socks` proxies return `None`, which leaves
/// the dial exactly as it always was.
fn parse_proxy(raw: &str) -> Option<Proxy> {
    let raw = raw.trim();
    let rest = match raw.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") => rest,
        Some(_) => return None,
        None => raw,
    };
    let authority = rest.split('/').next()?;
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((userinfo, hostport)) => (Some(userinfo), hostport),
        None => (None, authority),
    };
    let (host, port) = if let Some(v6) = hostport.strip_prefix('[') {
        let (host, after) = v6.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(port) => port.parse().ok()?,
            None if after.is_empty() => 80,
            None => return None,
        };
        (host.to_string(), port)
    } else {
        match hostport.rsplit_once(':') {
            Some((host, port)) => (host.to_string(), port.parse().ok()?),
            None => (hostport.to_string(), 80),
        }
    };
    if host.is_empty() {
        return None;
    }
    let authorization = userinfo.map(|userinfo| {
        let decoded = percent_decode(userinfo);
        format!("Basic {}", base64_encode(decoded.as_bytes()))
    });
    Some(Proxy {
        host,
        port,
        authorization,
    })
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = input.get(i + 1..i + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Standard base64 (RFC 4648 §4), for the one header that needs it.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> shift) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Open a TCP tunnel to `host:port` through `proxy` with HTTP `CONNECT`
/// (RFC 9110 §9.3.6). The caller runs TLS and the WebSocket handshake over
/// the returned stream exactly as over a direct one.
async fn tunnel(proxy: &Proxy, host: &str, port: u16) -> io::Result<TcpStream> {
    let mut stream = direct(&proxy.host, proxy.port).await?;
    let target = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
    if let Some(authorization) = &proxy.authorization {
        request.push_str(&format!("Proxy-Authorization: {authorization}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut head = Vec::with_capacity(256);
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= 8192 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "proxy CONNECT reply header too long",
            ));
        }
        // One byte at a time on purpose: anything past the blank line belongs
        // to the TLS handshake that follows, and must not be buffered away.
        head.push(stream.read_u8().await?);
    }
    let head = String::from_utf8_lossy(&head);
    let status_line = head.lines().next().unwrap_or_default();
    match status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
    {
        Some(200..=299) => Ok(stream),
        _ => Err(io::Error::other(format!(
            "proxy {} refused CONNECT to {target}: {status_line}",
            proxy.endpoint()
        ))),
    }
}

/// Race TCP connects over an already-ordered address list: one new attempt per
/// [`STAGGER`] tick (immediately on a failure), first success wins, losers are
/// dropped mid-flight.
async fn race_connect(mut queue: VecDeque<SocketAddr>) -> io::Result<TcpStream> {
    let Some(first) = queue.pop_front() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no addresses resolved",
        ));
    };
    let mut pending = FuturesUnordered::new();
    pending.push(TcpStream::connect(first));
    let stagger = tokio::time::sleep(STAGGER);
    tokio::pin!(stagger);

    loop {
        tokio::select! {
            res = pending.next(), if !pending.is_empty() => match res {
                Some(Ok(stream)) => return Ok(stream),
                Some(Err(err)) => {
                    // A failure advances the schedule immediately (§5).
                    match queue.pop_front() {
                        Some(addr) => {
                            pending.push(TcpStream::connect(addr));
                            stagger.as_mut().reset(tokio::time::Instant::now() + STAGGER);
                        }
                        // Last attempt standing: its error is the verdict.
                        None if pending.is_empty() => return Err(err),
                        None => {}
                    }
                }
                None => unreachable!("guarded on !pending.is_empty()"),
            },
            _ = stagger.as_mut(), if !queue.is_empty() => {
                if let Some(addr) = queue.pop_front() {
                    pending.push(TcpStream::connect(addr));
                }
                stagger.as_mut().reset(tokio::time::Instant::now() + STAGGER);
            }
        }
    }
}

/// Alternate address families starting with the resolver's first pick,
/// preserving resolver order within each family (RFC 8305 §4).
fn interleave_families(addrs: Vec<SocketAddr>) -> VecDeque<SocketAddr> {
    let first_is_v6 = addrs.first().is_some_and(SocketAddr::is_ipv6);
    let (preferred, other): (Vec<_>, Vec<_>) =
        addrs.into_iter().partition(|a| a.is_ipv6() == first_is_v6);
    let mut out = VecDeque::with_capacity(preferred.len() + other.len());
    let (mut preferred, mut other) = (preferred.into_iter(), other.into_iter());
    loop {
        match (preferred.next(), other.next()) {
            (None, None) => break,
            (a, b) => {
                out.extend(a);
                out.extend(b);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    fn proxy(host: &str, port: u16) -> Option<Proxy> {
        Some(Proxy {
            host: host.into(),
            port,
            authorization: None,
        })
    }

    #[test]
    fn a_wss_dial_uses_the_same_variables_reqwest_does() {
        let edge = "edge.letscypher.app";
        let vars = [
            ("HTTPS_PROXY", "http://proxy.corp:3128"),
            ("HTTP_PROXY", "http://plain:8080"),
        ];
        assert_eq!(
            proxy_for(Some("wss"), edge, 443, env(&vars)),
            proxy("proxy.corp", 3128)
        );
        assert_eq!(
            proxy_for(Some("ws"), edge, 80, env(&vars)),
            proxy("plain", 8080)
        );
        // Lower case works, and ALL_PROXY is the fallback for either scheme.
        let lower = [("https_proxy", "proxy.corp:3128")];
        assert_eq!(
            proxy_for(Some("wss"), edge, 443, env(&lower)),
            proxy("proxy.corp", 3128)
        );
        let all = [("ALL_PROXY", "http://everything:9000")];
        assert_eq!(
            proxy_for(Some("wss"), edge, 443, env(&all)),
            proxy("everything", 9000)
        );
        // Unset or blank: dial directly, exactly as before.
        assert_eq!(proxy_for(Some("wss"), edge, 443, env(&[])), None);
        assert_eq!(
            proxy_for(Some("wss"), edge, 443, env(&[("HTTPS_PROXY", "  ")])),
            None
        );
    }

    #[test]
    fn loopback_is_never_proxied() {
        let vars = [("ALL_PROXY", "http://proxy.corp:3128")];
        for host in ["localhost", "127.0.0.1", "::1", "dev.localhost"] {
            assert_eq!(
                proxy_for(Some("ws"), host, 27640, env(&vars)),
                None,
                "{host}"
            );
        }
    }

    #[test]
    fn no_proxy_excludes_hosts_domains_addresses_and_ports() {
        let edge = "edge.letscypher.app";
        let with = |list: &str, host: &str, port: u16| {
            let vars = [("HTTPS_PROXY", "http://p:1"), ("NO_PROXY", list)];
            proxy_for(Some("wss"), host, port, env(&vars)).is_none()
        };
        assert!(with("*", edge, 443));
        assert!(with("edge.letscypher.app", edge, 443));
        assert!(
            with("letscypher.app", edge, 443),
            "a domain covers its subdomains"
        );
        assert!(
            with(".letscypher.app", edge, 443),
            "a leading dot is optional"
        );
        assert!(with("  other.com , letscypher.app ", edge, 443));
        assert!(
            !with("cypher.app", edge, 443),
            "suffix match is by label, not by string"
        );
        assert!(!with("other.com", edge, 443));
        assert!(with("letscypher.app:443", edge, 443));
        assert!(
            !with("letscypher.app:8443", edge, 443),
            "a pinned port must match"
        );
        assert!(with("10.1.2.3", "10.1.2.3", 443));
        assert!(with("10.0.0.0/8", "10.1.2.3", 443));
        assert!(!with("10.0.0.0/8", "11.1.2.3", 443));
        assert!(with("2001:db8::/32", "2001:db8::5", 443));
    }

    #[test]
    fn proxy_urls_parse_like_their_http_counterparts() {
        assert_eq!(parse_proxy("http://proxy:3128/"), proxy("proxy", 3128));
        assert_eq!(parse_proxy("proxy:3128"), proxy("proxy", 3128));
        assert_eq!(parse_proxy("http://proxy"), proxy("proxy", 80));
        assert_eq!(parse_proxy("HTTP://Proxy:3128"), proxy("Proxy", 3128));
        assert_eq!(
            parse_proxy("http://[2001:db8::1]:8080"),
            proxy("2001:db8::1", 8080)
        );
        // Only plain HTTP proxies can be tunnelled; others keep the old path.
        for unsupported in [
            "https://p:443",
            "socks5://p:1080",
            "socks5h://p:1080",
            "http://",
            "",
        ] {
            assert_eq!(parse_proxy(unsupported), None, "{unsupported}");
        }
        // Credentials are percent-decoded, then sent as Basic auth.
        let authed = parse_proxy("http://us%40er:p%3Ass@proxy:3128").unwrap();
        assert_eq!(authed.host, "proxy");
        assert_eq!(
            authed.authorization.as_deref(),
            Some(format!("Basic {}", base64_encode(b"us@er:p:ss")).as_str())
        );
        // ...and never shown where a log could carry them.
        assert_eq!(authed.endpoint(), "proxy:3128");
    }

    #[test]
    fn base64_matches_rfc_4648_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(input.as_bytes()), expected, "{input}");
        }
    }

    /// A minimal CONNECT proxy: answers the tunnel request with `reply`, and
    /// on a 2xx relays bytes to `target`. Returns the request head it saw.
    async fn fake_proxy(
        target: u16,
        reply: &'static str,
    ) -> (u16, tokio::sync::oneshot::Receiver<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut client, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(client.read_u8().await.unwrap());
            }
            let _ = seen_tx.send(String::from_utf8_lossy(&head).into_owned());
            client.write_all(reply.as_bytes()).await.unwrap();
            if reply.starts_with("HTTP/1.1 2") {
                let mut upstream = TcpStream::connect(("127.0.0.1", target)).await.unwrap();
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            }
        });
        (port, seen_rx)
    }

    #[tokio::test]
    async fn a_websocket_runs_through_a_connect_tunnel() {
        use futures::SinkExt;
        use tokio_tungstenite::tungstenite::Message;
        // An echo WebSocket server, reachable only through the proxy.
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_port = server.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (stream, _) = server.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(message)) = ws.next().await {
                if message.is_text() {
                    ws.send(message).await.unwrap();
                }
            }
        });
        let (proxy_port, seen) =
            fake_proxy(ws_port, "HTTP/1.1 200 Connection Established\r\n\r\n").await;
        let proxy = Proxy {
            host: "127.0.0.1".into(),
            port: proxy_port,
            authorization: Some(format!("Basic {}", base64_encode(b"u:p"))),
        };
        let stream = tunnel(&proxy, "127.0.0.1", ws_port).await.unwrap();
        let head = seen.await.unwrap();
        assert!(
            head.starts_with(&format!("CONNECT 127.0.0.1:{ws_port} HTTP/1.1\r\n")),
            "{head}"
        );
        assert!(head.contains(&format!("Host: 127.0.0.1:{ws_port}\r\n")));
        assert!(head.contains("Proxy-Authorization: Basic dTpw\r\n"));
        let (mut ws, _) =
            tokio_tungstenite::client_async(format!("ws://127.0.0.1:{ws_port}/"), stream)
                .await
                .expect("the WebSocket handshake completes through the tunnel");
        ws.send(Message::text("through the proxy")).await.unwrap();
        let echoed = ws.next().await.unwrap().unwrap();
        assert_eq!(echoed.into_text().unwrap().as_str(), "through the proxy");
    }

    #[tokio::test]
    async fn a_refused_tunnel_says_why() {
        let (proxy_port, _seen) =
            fake_proxy(1, "HTTP/1.1 407 Proxy Authentication Required\r\n\r\n").await;
        let proxy = Proxy {
            host: "127.0.0.1".into(),
            port: proxy_port,
            authorization: None,
        };
        let err = tunnel(&proxy, "edge.example", 443).await.unwrap_err();
        let text = err.to_string();
        assert!(text.contains("407"), "{text}");
        assert!(text.contains("edge.example:443"), "{text}");
    }

    #[test]
    fn interleaves_families_from_resolver_first_pick() {
        let ordered = interleave_families(vec![
            addr("[2001:db8::1]:443"),
            addr("[2001:db8::2]:443"),
            addr("192.0.2.1:443"),
            addr("192.0.2.2:443"),
            addr("192.0.2.3:443"),
        ]);
        let got: Vec<_> = ordered.into_iter().collect();
        assert_eq!(
            got,
            vec![
                addr("[2001:db8::1]:443"),
                addr("192.0.2.1:443"),
                addr("[2001:db8::2]:443"),
                addr("192.0.2.2:443"),
                addr("192.0.2.3:443"),
            ]
        );
    }

    #[test]
    fn single_family_passes_through() {
        let ordered = interleave_families(vec![addr("192.0.2.1:80"), addr("192.0.2.2:80")]);
        let got: Vec<_> = ordered.into_iter().collect();
        assert_eq!(got, vec![addr("192.0.2.1:80"), addr("192.0.2.2:80")]);
    }

    #[tokio::test]
    async fn blackholed_first_address_does_not_block_the_working_one() {
        // A listener that accepts (the "IPv4 works" side) behind an RFC 5737
        // TEST-NET address that never answers (the blackholed side). The race
        // must connect via the listener in ~STAGGER instead of hanging on the
        // black hole the way a sequential dial does.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let queue: VecDeque<SocketAddr> = vec![
            addr("192.0.2.1:9"),
            format!("127.0.0.1:{port}").parse().unwrap(),
        ]
        .into();
        let started = std::time::Instant::now();
        let stream = tokio::time::timeout(Duration::from_secs(5), race_connect(queue))
            .await
            .expect("raced connect hung on the blackholed address")
            .unwrap();
        assert_eq!(stream.peer_addr().unwrap().port(), port);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "raced connect took {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn all_failures_surface_the_last_error() {
        // Two closed ports on loopback: both refuse, the error must surface
        // rather than the loop hanging or panicking.
        let a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (pa, pb) = (
            a.local_addr().unwrap().port(),
            b.local_addr().unwrap().port(),
        );
        drop((a, b));
        let queue: VecDeque<SocketAddr> = vec![
            format!("127.0.0.1:{pa}").parse().unwrap(),
            format!("127.0.0.1:{pb}").parse().unwrap(),
        ]
        .into();
        let err = tokio::time::timeout(Duration::from_secs(5), race_connect(queue))
            .await
            .expect("failed race must resolve promptly")
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::ConnectionRefused);
    }
}
