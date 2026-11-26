// A file preview in a reply (MDBlock.filePreview, desktop file_card.rs): a
// paragraph that is only a path to a csv/tsv, pdf, video or html file. A
// table shows its first rows in a card; a PDF its first pages side by side
// in one row and a video a frame with a play mark, each exactly the
// column's width like an image (PreviewSize.swift: a frame taller than 4:5
// is cropped, keeping its top; pages in a row never are), its name and
// detail under it; an html page a full-width card naming it. Tapping opens
// the file where it shows best: the table in Files, the PDF in a PDF
// viewer, the video in the player, the page in the artifact viewer.
// Anything that can't show reads as the same card saying why.
//
// The look is the system's: a grouped-background card with no outline, a
// table inset on it with the concentric inner radius, neutral ink. A
// visual preview isn't inset on a card (that made it narrower than an
// image): it is a picture rounded under a hairline edge (`previewPicture`).

import AVKit
import PDFKit
import SwiftUI

/// The card geometry every file preview shares: the inner radius is the
/// outer one less the padding, so the corners stay concentric.
enum FileCardMetrics {
    static let radius: CGFloat = 16
    static let padding: CGFloat = 6
    static let innerRadius: CGFloat = radius - padding
    /// The widest a card that doesn't fill the column (a table, a note
    /// saying why a file can't show) gets.
    static let maxWidth: CGFloat = 420
    /// The icon well in the footer row, inset by `padding` too.
    static let well: CGFloat = 34
}

extension FilePreview.Kind {
    /// The SF Symbol a card shows for the kind.
    var symbol: String {
        switch self {
        case .csv, .tsv: "tablecells"
        case .pdf: "doc.richtext"
        case .video: "play.rectangle"
        case .html: "globe"
        case .svg: "photo"
        }
    }
}

struct FilePreviewBlock: View {
    let src: String
    let kind: FilePreview.Kind

    @Environment(\.transcriptImageFiles) private var files
    @Environment(\.openWorkspace) private var openWorkspace
    @State private var outcome: Result<LoadedFilePreview, FilePreviewError>?
    @State private var page: ArtifactPage?
    @State private var pdf: LoadedFilePreview?
    @State private var video: PlayingVideo?
    @State private var fetching = false

    private var name: String { FilePreview.fileName(src) }

    var body: some View {
        let current =
            outcome
            ?? FilePreviewLoader.shared.cached(src, files: files).map { .success($0) }
            ?? FilePreviewLoader.shared.failure(src, files: files).map { .failure($0) }
        Group {
            switch current {
            case .success(let loaded):
                loadedCard(loaded)
            case .failure(let error):
                FileCard {
                    FileCardRow(
                        symbol: kind.symbol, title: name, detail: error.localizedDescription,
                        trailing: error.retryable ? .retry(retry) : .none)
                }
                .accessibilityIdentifier("file-preview-error")
            case nil:
                FileCard {
                    FileCardRow(symbol: kind.symbol, title: name, detail: "Loading…", trailing: .progress)
                }
                .accessibilityIdentifier("file-preview-loading")
            }
        }
        .task(id: "\(files?.scope ?? "")\u{1}\(src)") {
            if current == nil { await load() }
        }
        .fullScreenCover(item: $page) { ArtifactViewer(page: $0) }
        .fullScreenCover(item: $pdf) { PDFPreviewViewer(preview: $0) }
        .fullScreenCover(item: $video) { VideoPreviewPlayer(url: $0.url, title: name) }
    }

    private func load() async {
        let result = await FilePreviewLoader.shared.load(src, kind: kind, files: files)
        guard !Task.isCancelled else { return }
        outcome = result
    }

    private func retry() {
        FilePreviewLoader.shared.forget(src, files: files)
        outcome = nil
        Task { await load() }
    }

    // MARK: Loaded

    @ViewBuilder
    private func loadedCard(_ loaded: LoadedFilePreview) -> some View {
        switch loaded.body {
        case .table(let table):
            button(enabled: openWorkspace != nil, open: openInFiles) {
                FileCard {
                    FileTableView(table: table)
                        .clipShape(RoundedRectangle(cornerRadius: FileCardMetrics.innerRadius, style: .continuous))
                    FileCardRow(symbol: kind.symbol, title: loaded.name, detail: loaded.detail)
                }
            }
            .accessibilityLabel("\(loaded.name), table, \(loaded.detail)")
            .accessibilityHint(openWorkspace != nil ? "Opens the file" : "")
        case .frame(let image):
            button(open: { open(loaded) }) {
                VisualPreview {
                    VideoFrame(image: image)
                    FileCardRow(
                        symbol: kind.symbol, title: loaded.name, detail: loaded.detail,
                        trailing: fetching ? .progress : .none)
                }
            }
            .accessibilityLabel("\(loaded.name), video, \(loaded.detail)")
            .accessibilityHint("Plays the video")
        case .pages(let sizes):
            button(open: { open(loaded) }) {
                VisualPreview {
                    if let file = loaded.file {
                        PDFPageRow(file: file, sizes: sizes, pages: loaded.pages ?? sizes.count)
                    }
                    FileCardRow(symbol: kind.symbol, title: loaded.name, detail: loaded.detail)
                }
            }
            .accessibilityLabel("\(loaded.name), PDF, \(loaded.detail)")
            .accessibilityHint("Opens the PDF")
        case .page(let title, _):
            button(open: { open(loaded) }) {
                FileCard(fills: true) {
                    FileCardRow(
                        symbol: kind.symbol, title: title ?? loaded.name,
                        detail: title == nil ? loaded.detail : "\(loaded.name) · \(loaded.detail)", trailing: .open)
                }
            }
            .accessibilityLabel("\(title ?? loaded.name), \(loaded.detail)")
            .accessibilityHint("Opens the page")
        case .plain:
            button(enabled: loaded.kind == .video || loaded.file != nil, open: { open(loaded) }) {
                FileCard {
                    FileCardRow(
                        symbol: kind.symbol, title: loaded.name, detail: loaded.detail,
                        trailing: fetching ? .progress : (loaded.kind == .video ? .play : .none))
                }
            }
            .accessibilityLabel("\(loaded.name), \(loaded.detail)")
        }
    }

    private func button<Label: View>(
        enabled: Bool = true, open: @escaping () -> Void, @ViewBuilder label: () -> Label
    ) -> some View {
        Button(action: open, label: label)
            .buttonStyle(FileCardButtonStyle())
            .disabled(!enabled)
            .accessibilityIdentifier("file-preview-\(kind.rawValue)")
    }

    private func openInFiles() {
        let path = ImageSource.localPath(src) ?? src
        openWorkspace?(.reference(WorkspaceReference(path: path)))
    }

    private func open(_ loaded: LoadedFilePreview) {
        switch (loaded.kind, loaded.body) {
        case (_, .page(let title, let code)):
            page = ArtifactPage(kind: loaded.kind == .svg ? .svg : .html, title: title ?? loaded.name, code: code)
        case (.pdf, _):
            if loaded.file != nil { pdf = loaded }
        case (.video, _):
            if let file = loaded.file {
                video = PlayingVideo(url: file)
                return
            }
            // Too large to fetch for its frame alone: fetched now it's asked for.
            guard !fetching else { return }
            fetching = true
            Task {
                let result = await FilePreviewLoader.shared.load(src, kind: kind, files: files, whole: true)
                fetching = false
                switch result {
                case .success(let whole):
                    outcome = .success(whole)
                    video = whole.file.map(PlayingVideo.init)
                case .failure(let error):
                    outcome = .failure(error)
                }
            }
        default:
            break
        }
    }
}

/// The video file the player shows.
struct PlayingVideo: Identifiable {
    let url: URL
    var id: URL { url }
}

extension LoadedFilePreview: Identifiable {
    var id: ObjectIdentifier { ObjectIdentifier(self) }
}

// MARK: - Card

/// The grouped card: its content inset by `FileCardMetrics.padding`, the
/// rows stacked without gaps beyond that. An html page's card `fills`
/// the column; the rest stop at `FileCardMetrics.maxWidth`.
struct FileCard<Content: View>: View {
    var fills = false
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: FileCardMetrics.padding) {
            content
        }
        .padding(FileCardMetrics.padding)
        .frame(maxWidth: fills ? .infinity : FileCardMetrics.maxWidth, alignment: .leading)
        .background(
            Color(uiColor: .secondarySystemBackground),
            in: RoundedRectangle(cornerRadius: FileCardMetrics.radius, style: .continuous)
        )
        .contentShape(RoundedRectangle(cornerRadius: FileCardMetrics.radius, style: .continuous))
    }
}

/// A visual preview (a video's frame, a PDF's pages) and the row naming it
/// under it, not on a card: the picture is exactly the column's width, as
/// an image is.
struct VisualPreview<Content: View>: View {
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: FileCardMetrics.padding) {
            content
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
    }
}

/// A pressed card dims, as system list rows do.
struct FileCardButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .opacity(configuration.isPressed ? 0.6 : 1)
            .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
    }
}

/// The card's footer: the kind's icon in a well, the name and a detail
/// line, and what the trailing edge shows.
struct FileCardRow: View {
    enum Trailing {
        case none, open, play, progress
        case retry(() -> Void)
    }

    let symbol: String
    let title: String
    let detail: String
    var trailing: Trailing = .none

    @ScaledMetric(relativeTo: .subheadline) private var scaledWell = FileCardMetrics.well
    @ScaledMetric(relativeTo: .subheadline) private var scaledIcon: CGFloat = 15

    /// The well and its glyph grow with the text, up to a size that still
    /// leaves the name room.
    private var well: CGFloat { min(scaledWell, 48) }
    private var icon: CGFloat { min(scaledIcon, 22) }

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: symbol)
                .font(.system(size: icon, weight: .regular))
                .foregroundStyle(.secondary)
                .frame(width: well, height: well)
                .background(
                    Color(uiColor: .tertiarySystemFill),
                    in: RoundedRectangle(cornerRadius: FileCardMetrics.innerRadius, style: .continuous))
            VStack(alignment: .leading, spacing: 1) {
                Text(title)
                    .font(Theme.sans(MD.textSize, weight: .medium, relativeTo: .subheadline))
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(detail)
                    .font(Theme.sans(12, relativeTo: .caption))
                    .foregroundStyle(Theme.textMuted)
                    .lineLimit(2)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            switch trailing {
            case .none:
                EmptyView()
            case .open:
                Image(systemName: "chevron.right")
                    .font(.system(size: min(scaledIcon * 0.8, 17), weight: .semibold))
                    .foregroundStyle(.tertiary)
                    .padding(.trailing, 6)
            case .play:
                Image(systemName: "play.fill")
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
                    .padding(.trailing, 6)
            case .progress:
                ProgressView().controlSize(.small).padding(.trailing, 6)
            case .retry(let retry):
                Button(action: retry) {
                    Image(systemName: "arrow.clockwise")
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(.secondary)
                        .frame(width: 32, height: 32)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Try again")
            }
        }
        .accessibilityElement(children: .combine)
    }
}

// MARK: - Table

/// A csv/tsv head: the header row on a fill, data rows striped, in the
/// code face. Each column is as wide as its widest shown cell; wider than
/// the card, the table scrolls sideways.
struct FileTableView: View {
    let table: FilePreview.Table

    @ScaledMetric(relativeTo: .body) private var textSize: CGFloat = 12
    private static let cellPadding: CGFloat = 10

    var body: some View {
        let widths = columnWidths()
        ViewThatFits(in: .horizontal) {
            rows(widths, fill: true)
            ScrollView(.horizontal, showsIndicators: false) {
                rows(widths, fill: false)
            }
        }
        .background(Color(uiColor: .tertiarySystemBackground))
    }

    /// Each column's width in points: its widest cell in the code face.
    private func columnWidths() -> [CGFloat] {
        let font = Theme.monoUI(textSize, weight: .semibold)
        var widths = [CGFloat](repeating: 0, count: table.width)
        for row in (table.columns.map { [$0] } ?? []) + table.rows {
            for (column, cell) in row.enumerated() where column < widths.count {
                let width = (cell as NSString).size(withAttributes: [.font: font]).width
                widths[column] = max(widths[column], ceil(width))
            }
        }
        return widths.map { $0 + 2 * Self.cellPadding }
    }

    private func rows(_ widths: [CGFloat], fill: Bool) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            if let columns = table.columns {
                row(columns, widths, header: true, stripe: false, fill: fill)
            }
            ForEach(Array(table.rows.enumerated()), id: \.offset) { at, cells in
                row(cells, widths, header: false, stripe: at % 2 == 1, fill: fill)
            }
        }
    }

    private func row(
        _ cells: [String], _ widths: [CGFloat], header: Bool, stripe: Bool, fill: Bool
    ) -> some View {
        HStack(spacing: 0) {
            ForEach(Array(zip(cells, widths).enumerated()), id: \.offset) { _, pair in
                Text(pair.0)
                    .font(Theme.mono(12, weight: header ? .semibold : .regular))
                    .foregroundStyle(header ? Theme.text : Theme.text.opacity(0.88))
                    .lineLimit(1)
                    .padding(.horizontal, Self.cellPadding)
                    .frame(width: pair.1, alignment: .leading)
            }
            if fill { Spacer(minLength: 0) }
        }
        .padding(.vertical, 5)
        .background(
            Rectangle().fill(
                header
                    ? Color(uiColor: .tertiarySystemFill)
                    : stripe ? Color(uiColor: .quaternarySystemFill) : Color.clear))
    }
}

// MARK: - Thumbnails

/// A PDF's first pages side by side in one row filling the column
/// (`PreviewSize.pdfRow`): one height, never cropped, each rounded on white
/// under a hairline edge; a single page shows like an image (cropped to 4:5,
/// its top kept). Pages left over are a neutral "+N" badge on the last
/// shown one. Each page is drawn at the widest it gets in any column up to
/// this one (`PreviewSize.pdfDrawWidths`) at the screen's scale, so it stays
/// sharp. A tap on any page opens the file (the enclosing button).
struct PDFPageRow: View {
    let file: URL
    /// The first pages' sizes in points (`PreviewSize.pdfRowMax` at most).
    let sizes: [CGSize]
    /// The document's page count.
    let pages: Int

    /// Drawings are made for the column rounded up to this, so a small change
    /// in it doesn't draw them again.
    static let widthStep: CGFloat = 128

    @Environment(\.displayScale) private var displayScale
    @State private var width: CGFloat = 0
    @State private var drawn: PDFPageImages.Drawn?

    private var aspects: [CGFloat] { sizes.map { $0.width / $0.height } }

    /// Each page's drawing width in pixels.
    private var pixelWidths: [CGFloat] {
        guard width > 0 else { return [] }
        let column = (width / Self.widthStep).rounded(.up) * Self.widthStep
        return PreviewSize.pdfDrawWidths(column: column, aspects: aspects, pages: pages)
            .map { ($0 * displayScale).rounded(.up) }
    }

    var body: some View {
        let shown = drawn ?? PDFPageImages.shared.cached(file)
        let row = PreviewSize.pdfRow(column: width, aspects: aspects, pages: pages)
        PDFRowLayout(aspects: aspects, pages: pages) {
            ForEach(Array(sizes.enumerated()), id: \.offset) { index, size in
                PDFPageTile(
                    image: shown.flatMap { index < $0.images.count ? $0.images[index] : nil },
                    aspect: size.width / size.height,
                    badge: index == row.shown - 1 ? row.badge : nil)
            }
        }
        .onGeometryChange(for: CGFloat.self) {
            $0.size.width
        } action: {
            width = $0
        }
        .task(id: pixelWidths) {
            let targets = pixelWidths
            guard !targets.isEmpty, !(shown?.covers(targets) ?? false) else { return }
            let result = await PDFPageImages.shared.draw(file, pixelWidths: targets)
            guard !Task.isCancelled, let result else { return }
            drawn = result
        }
        .accessibilityIdentifier("pdf-pages")
    }
}

/// One page of a PDF's row: white, the page drawn its frame's width from
/// the top (a single tall page is cut off below), rounded under a hairline
/// edge, with the row's "+N" badge when it is the last shown page.
struct PDFPageTile: View {
    let image: UIImage?
    /// The page's width / height.
    let aspect: CGFloat
    let badge: String?

    var body: some View {
        Color.white
            .overlay(alignment: .top) {
                if let image {
                    Image(uiImage: image)
                        .resizable()
                        .interpolation(.high)
                        .aspectRatio(aspect, contentMode: .fill)
                }
            }
            .previewPicture()
            .overlay(alignment: .bottomTrailing) {
                if let badge {
                    PDFMorePagesBadge(label: badge)
                        .padding(8)
                }
            }
    }
}

/// "+3" on a PDF's last shown page: neutral ink on any page, light or dark
/// mode (the page is always white).
struct PDFMorePagesBadge: View {
    let label: String

    var body: some View {
        Text(label)
            .font(Theme.sans(12, weight: .semibold, relativeTo: .caption))
            .monospacedDigit()
            .foregroundStyle(.white)
            .padding(.horizontal, 8)
            .padding(.vertical, 2)
            .background(Color.black.opacity(0.6), in: Capsule())
            .accessibilityIdentifier("pdf-more-pages")
    }
}

/// A video's frame, the column's width (a portrait one cropped to 4:5,
/// keeping its top), with a play mark on a material.
struct VideoFrame: View {
    let image: UIImage

    var body: some View {
        PreviewFrame(natural: image.size) {
            Image(uiImage: image)
                .resizable()
                .interpolation(.high)
        }
        .background(Color.black)
        .overlay {
            Image(systemName: "play.fill")
                .font(.system(size: 18, weight: .semibold))
                .foregroundStyle(.white)
                .frame(width: 46, height: 46)
                .background(.ultraThinMaterial, in: Circle())
                .environment(\.colorScheme, .dark)
                .accessibilityHidden(true)
        }
        .previewPicture()
    }
}

// MARK: - Viewers

/// A PDF full screen (PDFKit), with Share.
struct PDFPreviewViewer: View {
    let preview: LoadedFilePreview

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Group {
                if let file = preview.file {
                    PDFKitView(url: file)
                } else {
                    Color.clear
                }
            }
            .ignoresSafeArea(edges: .bottom)
            .background(Color(uiColor: .systemGroupedBackground).ignoresSafeArea())
            .navigationTitle(preview.name)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button("Close", systemImage: "xmark") { dismiss() }
                        .accessibilityIdentifier("pdf-viewer-close")
                }
                ToolbarItem(placement: .primaryAction) {
                    if let file = preview.file {
                        ShareLink(item: file) { Image(systemName: "square.and.arrow.up") }
                    }
                }
            }
        }
        .accessibilityIdentifier("pdf-viewer")
    }
}

struct PDFKitView: UIViewRepresentable {
    let url: URL

    func makeUIView(context: Context) -> PDFView {
        let view = PDFView()
        view.autoScales = true
        view.displayMode = .singlePageContinuous
        view.backgroundColor = .systemGroupedBackground
        view.document = PDFDocument(url: url)
        return view
    }

    func updateUIView(_ view: PDFView, context: Context) {
        if view.document?.documentURL != url { view.document = PDFDocument(url: url) }
    }
}

/// A video full screen, playing from the start.
struct VideoPreviewPlayer: View {
    let url: URL
    let title: String

    @Environment(\.dismiss) private var dismiss
    @State private var player: AVPlayer?

    var body: some View {
        NavigationStack {
            VideoPlayer(player: player)
                .ignoresSafeArea(edges: .bottom)
                .background(Color.black.ignoresSafeArea())
                .navigationTitle(title)
                .navigationBarTitleDisplayMode(.inline)
                .toolbarBackground(.visible, for: .navigationBar)
                .toolbar {
                    ToolbarItem(placement: .topBarLeading) {
                        Button("Close", systemImage: "xmark") { dismiss() }
                            .accessibilityIdentifier("video-player-close")
                    }
                    ToolbarItem(placement: .primaryAction) {
                        ShareLink(item: url) { Image(systemName: "square.and.arrow.up") }
                    }
                }
        }
        .preferredColorScheme(.dark)
        .accessibilityIdentifier("video-player")
        .onAppear {
            let player = AVPlayer(url: url)
            self.player = player
            player.play()
        }
        .onDisappear { player?.pause() }
    }
}
