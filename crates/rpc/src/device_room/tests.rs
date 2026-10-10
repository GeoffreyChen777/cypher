//! Codec, relay-code and URL tests; the codec vectors are ported from
//! `edge/src/device-frame.test.ts`.

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

fn header(s: &str, k: &str) -> DeviceFrameHeader {
    DeviceFrameHeader::new(s, k)
}

#[test]
fn round_trips_header_and_payload() {
    // device-frame.test.ts: "round-trips header + payload"
    let payload = [1u8, 2, 3, 250, 255];
    let h = header("term-42", "term").with_to("conn-9");
    let frame = encode_device_frame(&h, &payload).expect("encode");
    let (decoded, out) = decode_device_frame(&frame).expect("decode");
    assert_eq!(decoded, h);
    assert_eq!(out, payload);
}

#[test]
fn handles_empty_payloads_and_long_headers() {
    // device-frame.test.ts: "handles empty payloads and long headers" — the 200-char
    // stream id forces a multi-byte uleb128 length prefix.
    let mut h = header(&"x".repeat(200), "rpc");
    h.from = Some("conn-1".into());
    let frame = encode_device_frame(&h, &[]).expect("encode");
    let json_len = serde_json::to_vec(&h).expect("json").len();
    assert!(json_len > 0x7f, "vector must exercise multi-byte uleb128");
    assert_eq!(frame[0], (json_len & 0x7f) as u8 | 0x80);
    assert_eq!(frame[1], (json_len >> 7) as u8);
    let (decoded, out) = decode_device_frame(&frame).expect("decode");
    assert_eq!(decoded, h);
    assert!(out.is_empty());
}

#[test]
fn byte_parity_with_ts_encoder() {
    // Byte-exact fixture computed from the TS encoder (uleb128 ‖ JSON.stringify
    // key order s,k,to,from ‖ payload).
    let frame = encode_device_frame(&header("a", "rpc"), &[1, 2]).expect("encode");
    let expected_json = br#"{"s":"a","k":"rpc"}"#;
    assert_eq!(frame[0] as usize, expected_json.len());
    assert_eq!(&frame[1..1 + expected_json.len()], expected_json);
    assert_eq!(&frame[1 + expected_json.len()..], &[1, 2]);

    let routed = encode_device_frame(&header("s1", "term").with_to("c9"), b"x").expect("encode");
    let expected = br#"{"s":"s1","k":"term","to":"c9"}"#;
    assert_eq!(routed[0] as usize, expected.len());
    assert_eq!(&routed[1..1 + expected.len()], expected);
}

#[test]
fn decodes_relay_control_payloads() {
    let payload = br#"{"error":"host_offline"}"#;
    assert_eq!(relay_error_code(payload).as_deref(), Some(HOST_OFFLINE));
    assert_eq!(relay_error_code(b"not json"), None);
}

#[test]
fn rejects_malformed_frames() {
    assert!(decode_device_frame(&[]).is_err()); // empty: truncated uleb128
    assert!(decode_device_frame(&[0x85]).is_err()); // continuation bit, no next byte
    assert!(decode_device_frame(&[10, b'{']).is_err()); // truncated header
    let mut minimal = vec![15u8];
    minimal.extend_from_slice(br#"{"s":"a","k":"b"}"#[..15].as_ref()); // wrong len: truncated JSON
    assert!(decode_device_frame(&minimal).is_err());
    let mut valid = vec![17u8];
    valid.extend_from_slice(br#"{"s":"a","k":"b"}"#);
    valid.push(9); // trailing payload byte
    let (h, p) = decode_device_frame(&valid).expect("valid minimal frame");
    assert_eq!((h.s.as_str(), h.k.as_str()), ("a", "b"));
    assert_eq!(p, vec![9]);
    assert!(decode_device_frame(&[0xff, 0xff, 0xff, 0xff, 0xff, 0x01]).is_err()); // overflow
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
