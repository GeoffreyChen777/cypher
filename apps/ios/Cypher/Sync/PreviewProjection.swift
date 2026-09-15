// Disposable display state. Never imports into Loro or advances its cursor.
import Foundation

struct PreviewCoverage: Codable, Equatable, Sendable {
    var runId: String
    var segmentId: String
    var epoch: String
    var revision: UInt64
    var complete: Bool
}

struct PreviewProjection {
    var grant: PreviewCoverage?
    var displayed: PreviewCoverage?
    var text = ""
    var interrupted = false
    var awaitingSnapshot = true
    private var resumeRequested = false
    private var receivedAt: Date?

    mutating func disconnect() { grant = nil; interrupted = true; resumeRequested = false }

    mutating func expire(now: Date = Date()) -> Bool {
        guard displayed != nil, let receivedAt else { return false }
        if now.timeIntervalSince(receivedAt) >= 300 { displayed = nil; text = ""; return true }
        if now.timeIntervalSince(receivedAt) >= 60 && !interrupted { interrupted = true; return true }
        return false
    }

    func retry(chatId: String, cursor: UInt64) -> Data? {
        guard awaitingSnapshot, let grant else { return nil }
        return StreamPreviewWire.encode(StreamPreviewWire.resume, header: ["chatId": chatId, "runId": grant.runId,
            "segmentId": grant.segmentId, "epoch": grant.epoch, "revision": displayed?.revision ?? 0, "baseSeq": cursor])
    }

    mutating func receive(_ data: Data, chatId: String) -> [Data] {
        guard let f = StreamPreviewWire.decode(data), f.header["chatId"] as? String == chatId else { return [] }
        if f.kind == StreamPreviewWire.state {
            if f.header["mode"] as? String == "preview" {
                grant = PreviewCoverage(runId: f.header["runId"] as! String, segmentId: f.header["segmentId"] as! String,
                                        epoch: f.header["epoch"] as! String, revision: 0, complete: false)
                displayed = nil; text = ""; interrupted = false; awaitingSnapshot = true; resumeRequested = false
            } else { disconnect() }
            return []
        }
        guard let grant, f.header["epoch"] as? String == grant.epoch,
              f.header["runId"] as? String == grant.runId, f.header["segmentId"] as? String == grant.segmentId,
              [StreamPreviewWire.delta, StreamPreviewWire.snapshot, StreamPreviewWire.finished].contains(f.kind) else { return [] }
        let revision = (f.header["revision"] as! NSNumber).uint64Value
        var replies: [Data] = []
        if f.kind != StreamPreviewWire.finished {
            if let displayed, revision < displayed.revision || (f.kind == StreamPreviewWire.delta && revision == displayed.revision) {
                // Duplicate receipt is still useful for transport credit.
            } else if f.kind == StreamPreviewWire.delta && (awaitingSnapshot || displayed?.revision != (f.header["prevRevision"] as? NSNumber)?.uint64Value) {
                awaitingSnapshot = true
                if !resumeRequested {
                    var h = f.header; h.removeValue(forKey: "prevRevision")
                    replies.append(StreamPreviewWire.encode(StreamPreviewWire.resume, header: h)!)
                    resumeRequested = true
                }
            } else {
                let incoming = String(data: f.payload, encoding: .utf8)!
                let next = f.kind == StreamPreviewWire.delta ? text + incoming : incoming
                guard next.utf8.count <= StreamPreviewWire.maxTextBytes else { disconnect(); return [] }
                text = next; displayed = grant; displayed?.revision = revision
                receivedAt = Date()
                interrupted = false; awaitingSnapshot = false; resumeRequested = false
            }
        }
        var h = f.header; h.removeValue(forKey: "prevRevision"); h.removeValue(forKey: "batchId")
        replies.append(StreamPreviewWire.encode(StreamPreviewWire.receipt, header: h)!)
        return replies
    }

}
