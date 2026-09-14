// UI-only adapter. Keep the protocol reducer independently testable.
import Foundation

extension PreviewProjection {
    func overlay(_ durable: [MessageEntry], coverage: PreviewCoverage?) -> [MessageEntry] {
        guard let displayed, !text.isEmpty else { return durable }
        let materialized = durable.contains(where: { $0.id == displayed.segmentId && $0.role == .assistant })
        if materialized, let coverage, coverage.runId == displayed.runId, coverage.segmentId == displayed.segmentId,
           coverage.epoch != displayed.epoch { return durable }
        let covered = materialized && coverage.map { $0.epoch == displayed.epoch && $0.runId == displayed.runId
                && $0.segmentId == displayed.segmentId && $0.revision >= displayed.revision } == true
        if covered && (interrupted || coverage?.complete == true) { return durable }
        var entries = durable
        let notice = interrupted ? "> 暂存预览 · 尚未确认同步\n\n" : "> 实时预览 · 本段结果待确认\n\n"
        let parts: [MessagePart] = [.text(id: "preview-text", text: notice + text)]
        if let i = entries.firstIndex(where: { $0.id == displayed.segmentId }) {
            guard entries[i].role == .assistant, entries[i].parts.allSatisfy({ if case .text = $0 { return true }; return false }) else { return durable }
            if interrupted {
                entries[i].parts.append(.text(id: "preview-status", text: "\n\n> 暂存预览 · 正在显示已同步内容，等待预览确认"))
            } else if covered {
                let current = entries[i].parts.compactMap { part -> String? in if case .text(_, let t) = part { return t }; return nil }.joined(separator: "\n")
                entries[i].parts = [.text(id: "preview-text", text: notice + current)]
            } else { entries[i].parts = parts; entries[i].status = .streaming }
        } else {
            entries.append(MessageEntry(id: displayed.segmentId, role: .assistant, parts: parts,
                                        createdAt: (entries.last?.createdAt ?? 0) + 1, deviceId: "", status: .streaming, continuationOf: nil))
        }
        return entries
    }
}
