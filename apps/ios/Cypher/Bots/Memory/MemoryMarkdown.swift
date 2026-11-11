// Markdown in a page row (a memory file, a held write's preview, an answer
// between workspaces): the transcript's blocks, laid out at their natural
// height so the List scrolls them. Parses are cached by text, so a store's
// frames, which resend every file, don't parse what hasn't changed.

import SwiftUI

struct MemoryMarkdown: View {
    let text: String
    /// Distinguishes highlight caches of different rows.
    var cacheKey = "memory-md"

    var body: some View {
        VStack(alignment: .leading, spacing: MD.blockGap) {
            ForEach(Array(MarkdownCache.blocks(text).enumerated()), id: \.offset) { ix, top in
                MarkdownBlockView(block: top.block, cacheKey: "\(cacheKey)/\(ix)")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// The last few parses, by source text.
@MainActor
private enum MarkdownCache {
    private static var entries: [String: [TopBlock]] = [:]
    private static var order: [String] = []
    private static let capacity = 48

    static func blocks(_ text: String) -> [TopBlock] {
        if let hit = entries[text] { return hit }
        let parsed = MarkdownParser.parse(text)
        entries[text] = parsed
        order.append(text)
        if order.count > capacity {
            entries[order.removeFirst()] = nil
        }
        return parsed
    }
}
