// Standalone Foundation runner: tests the exact iOS source without a simulator,
// network, app startup, credentials, or a copied fixture. Also exercised by XCTest.
import Foundation

@main
struct StreamPreviewVectors {
    static func main() throws {
        let data = try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))
        let vectors = try JSONSerialization.jsonObject(with: data) as! [[String: Any]]
        for v in vectors {
            let bytes: Data
            if let hex = v["hex"] as? String {
                let chars = Array(hex)
                bytes = Data(stride(from: 0, to: chars.count, by: 2).map {
                    UInt8(String(chars[$0...($0 + 1)]), radix: 16)!
                })
            } else {
                bytes = ChatWire.encode((v["kind"] as! NSNumber).uint8Value,
                                        header: v["header"] as! [String: Any],
                                        payload: Data((v["text"] as! String).utf8))
            }
            let decoded = StreamPreviewWire.decode(bytes)
            precondition((decoded != nil) == (v["valid"] as! Bool), v["name"] as! String)
            if let decoded {
                let text = String(data: decoded.payload, encoding: .utf8)!
                let encoded = StreamPreviewWire.encode(decoded.kind, header: decoded.header, text: text)!
                precondition(StreamPreviewWire.decode(encoded)?.payload == decoded.payload)
            }
        }
        let header: [String: Any] = ["chatId": "c", "runId": "r", "segmentId": "s", "epoch": "e", "revision": 0, "baseSeq": 0]
        precondition(StreamPreviewWire.encode(0x21, header: header, text: String(repeating: "x", count: 61_440)) != nil)
        precondition(StreamPreviewWire.encode(0x21, header: header, text: String(repeating: "界", count: 20_481)) == nil)
        precondition(StreamPreviewWire.decode(ChatWire.encode(0x21, header: header, payload: Data([0xff]))) == nil)
        precondition(StreamPreviewWire.decode(Data(repeating: 0, count: 65_537)) == nil)
        print("Swift preview: \(vectors.count) shared vectors + byte/UTF-8 limits passed")
        if CommandLine.arguments.count > 2 {
            let data = try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[2]))
            let cases = try JSONSerialization.jsonObject(with: data) as! [[String: Any]]
            var reducer = PreviewProjection()
            for c in cases {
                let bytes = StreamPreviewWire.encode((c["kind"] as! NSNumber).uint8Value,
                                                      header: c["header"] as! [String: Any], text: c["text"] as! String)!
                let replies = reducer.receive(bytes, chatId: "c")
                precondition((reducer.displayed == nil ? "" : reducer.text) == c["display"] as! String)
                precondition((reducer.displayed != nil && reducer.interrupted) == c["interrupted"] as! Bool)
                precondition(replies.map { Int($0[0]) } == c["replies"] as! [Int])
            }
            print("Swift preview: \(cases.count) shared state transitions passed")
        }
    }
}
