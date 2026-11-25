// A file preview in a reply (MDBlock.filePreview, desktop file_card.rs): a
// paragraph that is only a path to a csv/tsv, pdf, video or html file. A
// table shows its first rows, a PDF its first page and a video a frame with
// a play mark, an html page a card naming it. Tapping opens the file where
// it shows best: the table in Files, the PDF in a PDF viewer, the video in
// the player, the page in the artifact viewer. Anything that can't show
// reads as the same card saying why.
//
// The look is the system's: a grouped-background card with no outline, the
// preview inset on it with the concentric inner radius, neutral ink.

import AVKit
import PDFKit
import SwiftUI

/// The card geometry every file preview shares: the inner radius is the
/// outer one less the padding, so the corners stay concentric.
enum FileCardMetrics {
    static let radius: CGFloat = 16
    static let padding: CGFloat = 6
    static let innerRadius: CGFloat = radius - padding
    static let maxWidth: CGFloat = 420
    /// The icon well in the footer row, inset by `padding` too.
    static let well: CGFloat = 34
    /// The tallest a video frame shows; a PDF page's panel.
    static let frameMaxHeight: CGFloat = 240
    static let pagePanelHeight: CGFloat = 220
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
        case .thumb(let image):
            button(open: { open(loaded) }) {
                FileCard {
                    if loaded.kind == .video {
                        VideoFrame(image: image)
                    } else {
                        PDFPagePanel(image: image)
                    }
                    FileCardRow(
                        symbol: kind.symbol, title: loaded.name, detail: loaded.detail,
                        trailing: fetching ? .progress : .none)
                }
            }
            .accessibilityLabel("\(loaded.name), \(loaded.kind == .video ? "video" : "PDF"), \(loaded.detail)")
            .accessibilityHint(loaded.kind == .video ? "Plays the video" : "Opens the PDF")
        case .page(let title, _):
            button(open: { open(loaded) }) {
                FileCard {
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
/// rows stacked without gaps beyond that.
struct FileCard<Content: View>: View {
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: FileCardMetrics.padding) {
            content
        }
        .padding(FileCardMetrics.padding)
        .frame(maxWidth: FileCardMetrics.maxWidth, alignment: .leading)
        .background(
            Color(uiColor: .secondarySystemBackground),
            in: RoundedRectangle(cornerRadius: FileCardMetrics.radius, style: .continuous)
        )
        .contentShape(RoundedRectangle(cornerRadius: FileCardMetrics.radius, style: .continuous))
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

    @ScaledMetric(relativeTo: .subheadline) private var well = FileCardMetrics.well

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: symbol)
                .font(.system(size: 15, weight: .regular))
                .imageScale(.medium)
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
                    .font(.system(size: 12, weight: .semibold))
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
/// code face; wider than the card it scrolls sideways.
struct FileTableView: View {
    let table: FilePreview.Table

    var body: some View {
        ViewThatFits(in: .horizontal) {
            grid(fill: true)
            ScrollView(.horizontal, showsIndicators: false) {
                grid(fill: false)
            }
        }
        .background(Color(uiColor: .tertiarySystemBackground))
    }

    private func grid(fill: Bool) -> some View {
        Grid(alignment: .leading, horizontalSpacing: 0, verticalSpacing: 0) {
            if let columns = table.columns {
                row(columns, header: true, stripe: false)
            }
            ForEach(Array(table.rows.enumerated()), id: \.offset) { at, cells in
                row(cells, header: false, stripe: at % 2 == 1)
            }
        }
        .frame(maxWidth: fill ? .infinity : nil, alignment: .leading)
    }

    private func row(_ cells: [String], header: Bool, stripe: Bool) -> some View {
        GridRow {
            ForEach(Array(cells.enumerated()), id: \.offset) { _, cell in
                Text(cell.isEmpty ? " " : cell)
                    .font(Theme.mono(12, weight: header ? .semibold : .regular))
                    .foregroundStyle(header ? Theme.text : Theme.text.opacity(0.88))
                    .lineLimit(1)
                    .fixedSize()
                    .padding(.horizontal, 10)
                    .padding(.vertical, 5)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(
                        header
                            ? Color(uiColor: .tertiarySystemFill)
                            : stripe ? Color(uiColor: .quaternarySystemFill) : Color.clear)
            }
        }
    }
}

// MARK: - Thumbnails

/// A PDF's first page on a neutral panel, lifted by a soft shadow.
struct PDFPagePanel: View {
    let image: UIImage

    @ScaledMetric(relativeTo: .body) private var height = FileCardMetrics.pagePanelHeight

    var body: some View {
        Image(uiImage: image)
            .resizable()
            .interpolation(.high)
            .aspectRatio(contentMode: .fit)
            .clipShape(RoundedRectangle(cornerRadius: 3, style: .continuous))
            .shadow(color: .black.opacity(0.14), radius: 3, y: 1)
            .padding(14)
            .frame(maxWidth: .infinity)
            .frame(height: min(height, 360))
            .background(Color(uiColor: .tertiarySystemFill))
            .clipShape(RoundedRectangle(cornerRadius: FileCardMetrics.innerRadius, style: .continuous))
    }
}

/// A video's frame, letterboxed on black, with a play mark on a material.
struct VideoFrame: View {
    let image: UIImage

    var body: some View {
        let aspect = image.size.height > 0 ? image.size.width / image.size.height : 16 / 9
        Color.black
            .aspectRatio(max(aspect, 4 / 3), contentMode: .fit)
            .frame(maxWidth: .infinity, maxHeight: FileCardMetrics.frameMaxHeight)
            .overlay {
                Image(uiImage: image)
                    .resizable()
                    .aspectRatio(contentMode: .fit)
            }
            .overlay {
                Image(systemName: "play.fill")
                    .font(.system(size: 18, weight: .semibold))
                    .foregroundStyle(.white)
                    .frame(width: 46, height: 46)
                    .background(.ultraThinMaterial, in: Circle())
                    .environment(\.colorScheme, .dark)
                    .accessibilityHidden(true)
            }
            .clipShape(RoundedRectangle(cornerRadius: FileCardMetrics.innerRadius, style: .continuous))
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
