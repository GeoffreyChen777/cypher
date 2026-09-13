//! Cross-language RPC fragment corpus; only writes the caller's temporary file.
use cypher_rpc::workspace3::codec::{Decoder, Encoder};
use serde_json::{Value, json};

fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args[1] == "--write" {
        let value = json!({"text":"中🙂\n\"\\\u{0}".repeat(15_000),"null":null,"ok":true,"n":123});
        let parts: Vec<_> = Encoder::new(&value).unwrap().collect();
        std::fs::write(
            &args[2],
            serde_json::to_vec(&json!({"value":value,"parts":parts})).unwrap(),
        )
        .unwrap();
        println!("PASS: Rust wrote bounded RPC Unicode fragments");
    } else {
        assert_eq!(args[1], "--verify");
        let corpus: Value = serde_json::from_slice(&std::fs::read(&args[2]).unwrap()).unwrap();
        let mut decoder = Decoder::default();
        let parts = corpus["parts"].as_array().unwrap();
        for (i, part) in parts.iter().enumerate() {
            assert!(serde_json::to_vec(part).unwrap().len() < 64 * 1024);
            let result = decoder.push(part.clone()).unwrap();
            if i + 1 == parts.len() {
                assert_eq!(result.as_ref(), Some(&corpus["value"]));
            } else {
                assert_eq!(result, None);
            }
        }
        println!("PASS: Rust decoded the complete Swift RPC fragments");
    }
}
