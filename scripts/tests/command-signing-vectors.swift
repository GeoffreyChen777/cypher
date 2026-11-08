// Runs protocol/vectors/command-signing-v1.json against the iOS command
// signer (apps/ios/Cypher/Sync/CommandSignature.swift) without a simulator.
// The vectors come from crates/doc/tests/command_vectors.rs.
//
//   swiftc apps/ios/Cypher/Sync/CommandSignature.swift \
//     scripts/tests/command-signing-vectors.swift -o /tmp/command-vectors
//   /tmp/command-vectors protocol/vectors/command-signing-v1.json

import Foundation

func value(_ tagged: Any) -> CommandValue {
    let object = tagged as! [String: Any]
    precondition(object.count == 1, "one tag per value")
    let (tag, inner) = object.first!
    switch tag {
    case "null": return .null
    case "bool": return .bool(inner as! Bool)
    case "i64": return .i64(Int64(inner as! String)!)
    case "double": return .double((inner as! NSNumber).doubleValue)
    case "string": return .string(inner as! String)
    case "binary": return .binary(unhex(inner as! String))
    case "list": return .list((inner as! [Any]).map(value))
    case "map": return .map((inner as! [String: Any]).mapValues(value))
    default: fatalError("unknown tag \(tag)")
    }
}

func unhex(_ hex: String) -> Data {
    var out = Data()
    var index = hex.startIndex
    while index < hex.endIndex {
        let next = hex.index(index, offsetBy: 2)
        out.append(UInt8(hex[index..<next], radix: 16)!)
        index = next
    }
    return out
}

func hex(_ data: Data) -> String {
    data.map { String(format: "%02x", $0) }.joined()
}

@main
struct CommandSigningVectors {
    static func main() throws {
        let path = CommandLine.arguments[1]
        let file = try JSONSerialization.jsonObject(with: Data(contentsOf: URL(fileURLWithPath: path))) as! [String: Any]
        let cases = file["cases"] as! [[String: Any]]
        for testCase in cases {
            let fields = (testCase["fields"] as! [String: Any]).mapValues(value)
            let bytes = CommandSigning.signingBytes(chatId: testCase["chatId"] as! String, fields: fields)
            precondition(hex(bytes) == testCase["signingBytes"] as! String, "\(testCase["name"]!)")
        }
        print("Swift command signing: \(cases.count) cases match Rust")
    }
}
