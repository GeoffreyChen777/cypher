// How Files shows a file that isn't plain code (desktop files/preview.rs):
// Markdown rendered like a reply, HTML and SVG as pages, images fitted or at
// 100% with their format, pixel size and file size.

import SwiftUI
import UIKit

enum WorkspaceFileKind: Equatable {
    case text, markdown, html, svg, image

    init(path: String) {
        switch (path as NSString).pathExtension.lowercased() {
        case "md", "markdown", "mdown", "mkd": self = .markdown
        case "html", "htm", "xhtml": self = .html
        case "svg": self = .svg
        case "png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "ico", "heic", "heif": self = .image
        default: self = .text
        }
    }

    /// Opens rendered, with its source a toggle away.
    var previewable: Bool { self == .markdown || self == .html || self == .svg }
}

/// A text file rendered: Markdown as a reply reads, HTML and SVG as pages.
struct WorkspaceRenderedView: View {
    let kind: WorkspaceFileKind
    let text: String
    let partial: Bool

    var body: some View {
        VStack(spacing: 0) {
            if partial {
                Label("Preview truncated — showing loaded text", systemImage: "exclamationmark.triangle")
                    .font(Theme.sans(11)).foregroundStyle(Theme.warning)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 16).padding(.vertical, 10)
                    .background(Theme.surface)
            }
            switch kind {
            case .markdown:
                WorkspaceMarkdownView(text: text)
            case .html, .svg:
                // A page from another device loads as text with no base URL,
                // as on the desktop: relative stylesheets and images don't
                // load, and nothing it saves outlives the sheet.
                let page = ArtifactPage(
                    kind: kind == .svg ? .svg : .html, title: "", code: text)
                ArtifactWebView(html: ArtifactViewer.document(for: page))
                    .accessibilityIdentifier("workspace-page")
            case .text, .image:
                EmptyView()
            }
        }
    }
}

/// Markdown rendered with the transcript's blocks.
private struct WorkspaceMarkdownView: View {
    let text: String
    @State private var blocks: [TopBlock]?

    var body: some View {
        ScrollView {
            if let blocks {
                LazyVStack(alignment: .leading, spacing: MD.blockGap) {
                    ForEach(Array(blocks.enumerated()), id: \.offset) { ix, top in
                        MarkdownBlockView(block: top.block, cacheKey: "workspace-md/\(ix)")
                    }
                }
                .padding(16)
                .frame(maxWidth: TranscriptView.maxContentWidth, alignment: .leading)
                .frame(maxWidth: .infinity)
            }
        }
        .overlay {
            if blocks == nil { ProgressView() }
        }
        .accessibilityIdentifier("workspace-markdown")
        .task(id: text) {
            let source = text
            let parsed = await Task.detached(priority: .userInitiated) { MarkdownParser.parse(source) }.value
            guard !Task.isCancelled else { return }
            blocks = parsed
        }
    }
}

/// An image from the checkout: fitted to the sheet, or at 100% scrolling
/// both ways, over a caption with its format, pixel size and file size.
struct WorkspaceImageView: View {
    let session: WorkspaceBrowserSession
    let path: String
    var revision = 0
    @State private var image: UIImage?
    @State private var bytes = 0
    @State private var error: String?
    @State private var actualSize = false

    var body: some View {
        VStack(spacing: 0) {
            if let image {
                Group {
                    if actualSize {
                        ScrollView([.horizontal, .vertical]) {
                            Image(uiImage: image)
                                .interpolation(.none)
                                .frame(width: image.size.width, height: image.size.height)
                        }
                    } else {
                        Image(uiImage: image)
                            .resizable()
                            .interpolation(image.size.width < 128 ? .none : .high)
                            .scaledToFit()
                            .frame(
                                maxWidth: min(image.size.width, .infinity),
                                maxHeight: min(image.size.height, .infinity)
                            )
                            .frame(maxWidth: .infinity, maxHeight: .infinity)
                            .padding(16)
                    }
                }
                .background(checkerboard)
                .accessibilityLabel("Image, \(caption(image))")
                .accessibilityIdentifier("workspace-image")
                Text(caption(image))
                    .font(Theme.mono(11))
                    .foregroundStyle(Theme.textMuted)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 10)
                    .background(Theme.surface)
            } else if let error {
                ContentUnavailableView {
                    Label("Image unavailable", systemImage: "photo")
                } description: {
                    Text(error)
                }
            } else {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .toolbar {
            if image != nil {
                ToolbarItem(placement: .primaryAction) {
                    Button(
                        actualSize ? "Fit" : "Actual size",
                        systemImage: actualSize
                            ? "arrow.down.right.and.arrow.up.left" : "arrow.up.left.and.arrow.down.right"
                    ) {
                        actualSize.toggle()
                    }
                    .accessibilityIdentifier("workspace-image-size")
                }
            }
        }
        .task(id: "\(path)/\(revision)") {
            image = nil
            error = nil
            do {
                let data = try await session.fileBytes(path)
                guard !Task.isCancelled else { return }
                guard let decoded = UIImage(data: data, scale: 1) else {
                    error = "This file isn't an image iOS can show (\(Self.size(data.count)))."
                    return
                }
                bytes = data.count
                image = decoded
            } catch {
                guard !Task.isCancelled else { return }
                self.error = WorkspaceBrowserSession.errorMessage(error)
            }
        }
    }

    private func caption(_ image: UIImage) -> String {
        let format = (path as NSString).pathExtension.uppercased()
        let pixels = "\(Int(image.size.width)) × \(Int(image.size.height))"
        return "\(format) · \(pixels) · \(Self.size(bytes))"
    }

    private static func size(_ bytes: Int) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(bytes), countStyle: .file)
    }

    /// Transparency reads against a faint checkerboard.
    private var checkerboard: some View {
        Canvas { context, size in
            let cell: CGFloat = 10
            for row in 0..<Int(ceil(size.height / cell)) {
                for column in 0..<Int(ceil(size.width / cell)) where (row + column).isMultiple(of: 2) {
                    context.fill(
                        Path(CGRect(x: CGFloat(column) * cell, y: CGFloat(row) * cell, width: cell, height: cell)),
                        with: .color(Theme.text.opacity(0.04)))
                }
            }
        }
    }
}
