//! Codec, relay-code and URL tests. The codec runs the shared vectors in
//! `protocol/vectors/device-frames-v1.json`, which the TypeScript and Swift
//! codecs run too (`protocol/README.md`).

use super::frames::relay_error_code;
use super::links::credential_transport_allowed;
use super::*;

#[test]
fn credentials_require_tls_except_for_loopback_development() {
    for url in [
        "https://edge.example.com",
        "wss://edge.example.com",
        "http://127.0.0.1:1234",
        "http://localhost:1234",
        "ws://[::1]:1234",
    ] {
        assert!(credential_transport_allowed(url), "{url}");
    }
    for url in [
        "http://edge.example.com",
        "ws://192.168.1.2",
        "file:///tmp/relay",
        "https://user:key@edge.example.com",
        "https://edge.example.com?token=key",
        "https://edge.example.com#fragment",
        "not a url",
    ] {
        assert!(!credential_transport_allowed(url), "{url}");
    }
}

fn vectors() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../../protocol/vectors/device-frames-v1.json"
    ))
    .expect("device-frame vectors parse")
}

fn hex(v: &serde_json::Value) -> Vec<u8> {
    let s = v.as_str().expect("hex string");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex byte"))
        .collect()
}

#[test]
fn shared_vectors_encode_and_decode() {
    for c in vectors()["frames"].as_array().expect("frames") {
        let name = c["name"].as_str().expect("name");
        let header: DeviceFrameHeader = serde_json::from_value(c["header"].clone()).expect(name);
        assert_eq!(
            serde_json::to_string(&header).expect("json"),
            c["json"].as_str().expect("json"),
            "{name}: header key order"
        );
        let frame = encode_device_frame(&header, &hex(&c["payload"])).expect(name);
        assert_eq!(frame, hex(&c["hex"]), "{name}: encode");
        let (decoded, payload) = decode_device_frame(&frame).expect(name);
        assert_eq!(decoded, header, "{name}");
        assert_eq!(payload, hex(&c["payload"]), "{name}");
    }
}

#[test]
fn shared_vectors_reject_malformed() {
    for c in vectors()["malformed"].as_array().expect("malformed") {
        assert!(
            decode_device_frame(&hex(&c["hex"])).is_err(),
            "{}",
            c["name"]
        );
    }
}

#[test]
fn decodes_relay_control_payloads() {
    let payload = br#"{"error":"host_offline"}"#;
    assert_eq!(relay_error_code(payload).as_deref(), Some(HOST_OFFLINE));
    assert_eq!(relay_error_code(b"not json"), None);
}

#[test]
fn ws_url_shapes() {
    let url = device_room_ws_url(
        "https://edge.example/",
        "dev-1",
        "client",
        Some("c1"),
        "tok",
    );
    assert_eq!(
        url,
        "wss://edge.example/device/dev-1/ws?role=client&connId=c1&token=tok"
    );
    let host = device_room_ws_url("http://localhost:26640", "d", "host", None, "t");
    assert_eq!(host, "ws://localhost:26640/device/d/ws?role=host&token=t");
}
