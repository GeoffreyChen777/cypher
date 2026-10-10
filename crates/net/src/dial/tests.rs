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
    let (mut ws, _) = tokio_tungstenite::client_async(format!("ws://127.0.0.1:{ws_port}/"), stream)
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
