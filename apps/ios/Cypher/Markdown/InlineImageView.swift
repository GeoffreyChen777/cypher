// A chat image: the block preview a reply's `![alt](src)` (or a paragraph of
// only an image path) renders as, the thumbnails file chips and tool rows
// show, and the full-screen viewer a tap opens (pinch and double-tap zoom,
// Share). An SVG has no bitmap here: it shows as a card that opens the page
// in the artifact viewer's web view.

import SwiftUI
import UIKit

/// The block preview: fitted to the column, at most `maxHeight` tall and
/// never past the image's own size, with rounded corners and its alt text
/// as a small caption under it.
struct InlineImageBlock: View {
    let src: String
    let alt: String

    static let maxHeight: CGFloat = 360
    static let radius: CGFloat = 8

    @Environment(\.transcriptImageFiles) private var files
    @State private var outcome: Result<LoadedImage, ImageLoadError>?
    @State private var viewing: LoadedImage?
    @State private var page: ArtifactPage?

    /// The caption: the alt text (a paragraph that was only an image path
    /// has the path as its alt).
    private var caption: String? { alt.isEmpty ? nil : alt }

    /// What names the image where there's no caption: its file name.
    private var name: String { caption ?? ImageLoader.fileName(src) }

    var body: some View {
        let current =
            outcome
            ?? ImageLoader.shared.cached(src, files: files, size: .block).map { .success($0) }
            ?? ImageLoader.shared.failure(src, files: files).map { .failure($0) }
        VStack(alignment: .leading, spacing: 6) {
            switch current {
            case .success(let image):
                loaded(image)
            case .failure(let error):
                ImageErrorCard(
                    title: name, reason: error.localizedDescription,
                    retry: error.retryable
                        ? {
                            ImageLoader.shared.forget(src, files: files)
                            outcome = nil
                            Task { await load() }
                        } : nil)
            case nil:
                placeholder
            }
            if let caption, case .success(let image) = current, image.bitmap != nil {
                Text(caption)
                    .font(alt == src ? Theme.mono(11.5) : Theme.sans(12))
                    .foregroundStyle(Theme.textMuted)
                    .lineLimit(3)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .task(id: "\(files?.scope ?? "")\u{1}\(src)") {
            if current == nil { await load() }
        }
        .fullScreenCover(item: $viewing) { image in
            ImageViewer(image: image, src: src, title: name)
        }
        .fullScreenCover(item: $page) { ArtifactViewer(page: $0) }
    }

    private func load() async {
        let result = await ImageLoader.shared.load(src, files: files, size: .block)
        guard !Task.isCancelled else { return }
        outcome = result
    }

    @ViewBuilder
    private func loaded(_ image: LoadedImage) -> some View {
        switch image.content {
        case .bitmap(let bitmap):
            let size = Self.fitted(image.pixelSize)
            Button {
                viewing = image
            } label: {
                ImageBitmapView(image: bitmap)
                    .aspectRatio(size.width / max(size.height, 1), contentMode: .fit)
                    .frame(maxWidth: size.width)
                    .clipShape(RoundedRectangle(cornerRadius: Self.radius))
                    .overlay(
                        RoundedRectangle(cornerRadius: Self.radius)
                            .strokeBorder(FileReferenceChip.hairline.swiftUI, lineWidth: 1)
                    )
                    .contentShape(RoundedRectangle(cornerRadius: Self.radius))
            }
            .buttonStyle(.plain)
            .accessibilityLabel(alt.isEmpty ? "Image" : alt)
            .accessibilityHint("Opens the image")
            .accessibilityIdentifier("inline-image")
        case .svg(let text):
            SVGCard(title: name) {
                page = ArtifactPage(kind: .svg, title: name, code: text)
            }
        }
    }

    /// The largest size at most `maxHeight` tall and its own size wide; the
    /// column narrows it further, keeping the aspect ratio.
    static func fitted(_ pixels: CGSize) -> CGSize {
        guard pixels.width > 0, pixels.height > 0 else { return CGSize(width: maxHeight, height: maxHeight) }
        let scale = min(1, maxHeight / pixels.height)
        return CGSize(width: pixels.width * scale, height: pixels.height * scale)
    }

    private var placeholder: some View {
        RoundedRectangle(cornerRadius: Self.radius)
            .fill(FileReferenceChip.wash.swiftUI)
            .overlay(
                RoundedRectangle(cornerRadius: Self.radius)
                    .strokeBorder(FileReferenceChip.hairline.swiftUI, lineWidth: 1)
            )
            .overlay { ProgressView().controlSize(.small) }
            .frame(maxWidth: 320)
            .frame(height: 140)
            .accessibilityLabel("Loading image")
            .accessibilityIdentifier("inline-image-loading")
    }
}

extension UIColor {
    var swiftUI: Color { Color(uiColor: self) }
}

/// A still or animated bitmap, filling the frame it is given.
struct ImageBitmapView: View {
    let image: UIImage

    var body: some View {
        if (image.images?.count ?? 0) > 1 {
            AnimatedImageView(image: image)
        } else {
            Image(uiImage: image)
                .resizable()
                .interpolation(image.size.width < 128 ? .medium : .high)
        }
    }
}

/// An animated image (GIF, WebP) playing in a UIImageView: SwiftUI's Image
/// shows only one frame.
struct AnimatedImageView: UIViewRepresentable {
    let image: UIImage

    func makeUIView(context: Context) -> UIImageView {
        let view = UIImageView(image: image)
        view.contentMode = .scaleAspectFill
        view.clipsToBounds = true
        view.setContentHuggingPriority(.defaultLow, for: .horizontal)
        view.setContentHuggingPriority(.defaultLow, for: .vertical)
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        view.setContentCompressionResistancePriority(.defaultLow, for: .vertical)
        view.startAnimating()
        return view
    }

    func updateUIView(_ view: UIImageView, context: Context) {
        if view.image !== image {
            view.image = image
            view.startAnimating()
        }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView: UIImageView, context: Context) -> CGSize? {
        proposal.replacingUnspecifiedDimensions(by: image.size)
    }
}

/// An image that could not be shown: what it was, and why, with a retry.
struct ImageErrorCard: View {
    let title: String
    let reason: String
    var retry: (() -> Void)?

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "photo")
                .font(.system(size: 14))
                .foregroundStyle(Theme.textMuted)
                .frame(width: 20, height: 20)
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(Theme.sans(12.5, weight: .medium))
                    .foregroundStyle(Theme.text.opacity(0.85))
                    .lineLimit(2)
                    .truncationMode(.middle)
                Text(reason)
                    .font(Theme.sans(12))
                    .foregroundStyle(Theme.textMuted)
                    .lineLimit(3)
            }
            Spacer(minLength: 0)
            if let retry {
                Button(action: retry) {
                    Image(systemName: "arrow.clockwise")
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(Theme.textMuted)
                        .frame(width: 28, height: 28)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Try again")
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .frame(maxWidth: 420, alignment: .leading)
        .background(FileReferenceChip.wash.swiftUI, in: RoundedRectangle(cornerRadius: InlineImageBlock.radius))
        .overlay(
            RoundedRectangle(cornerRadius: InlineImageBlock.radius)
                .strokeBorder(FileReferenceChip.hairline.swiftUI, lineWidth: 1)
        )
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("inline-image-error")
    }
}

/// An SVG image: a card naming it, which opens it as a page.
struct SVGCard: View {
    let title: String
    let open: () -> Void

    var body: some View {
        Button(action: open) {
            HStack(spacing: 10) {
                Text("SVG")
                    .font(Theme.mono(10.5, weight: .semibold))
                    .foregroundStyle(Theme.textMuted)
                    .padding(.horizontal, 6)
                    .frame(height: 20)
                    .background(whiteAlpha(0.06), in: RoundedRectangle(cornerRadius: 5))
                Text(title)
                    .font(Theme.sans(12.5))
                    .foregroundStyle(Theme.text.opacity(0.85))
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: 0)
                Text("Open")
                    .font(Theme.sans(12, weight: .medium))
                    .foregroundStyle(Theme.textMuted)
            }
            .padding(.horizontal, 12)
            .frame(height: 40)
            .frame(maxWidth: 420)
            .contentShape(Rectangle())
        }
        .buttonStyle(PressWashButtonStyle(cornerRadius: InlineImageBlock.radius))
        .background(FileReferenceChip.wash.swiftUI, in: RoundedRectangle(cornerRadius: InlineImageBlock.radius))
        .overlay(
            RoundedRectangle(cornerRadius: InlineImageBlock.radius)
                .strokeBorder(FileReferenceChip.hairline.swiftUI, lineWidth: 1)
        )
        .accessibilityIdentifier("inline-image-svg")
    }
}

/// A small square thumbnail of `src` (tool rows), tappable to the viewer;
/// nothing while it loads or when it can't be shown.
struct ImageThumbnail: View {
    let src: String
    var side: CGFloat = 22

    @Environment(\.transcriptImageFiles) private var files
    @State private var image: LoadedImage?
    @State private var viewing: LoadedImage?
    @State private var page: ArtifactPage?

    var body: some View {
        let shown = image ?? ImageLoader.shared.cached(src, files: files, size: .thumbnail)
        Group {
            if let shown {
                Button {
                    Task { await open(shown) }
                } label: {
                    Group {
                        if let bitmap = shown.bitmap {
                            Image(uiImage: bitmap).resizable().scaledToFill()
                        } else {
                            Text("SVG").font(Theme.mono(7, weight: .semibold)).foregroundStyle(Theme.textMuted)
                        }
                    }
                    .frame(width: side, height: side)
                    .background(FileReferenceChip.wash.swiftUI)
                    .clipShape(RoundedRectangle(cornerRadius: 4))
                    .overlay(
                        RoundedRectangle(cornerRadius: 4).strokeBorder(FileReferenceChip.hairline.swiftUI, lineWidth: 1)
                    )
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Image preview")
                .accessibilityIdentifier("tool-image-thumbnail")
            }
        }
        .task(id: "\(files?.scope ?? "")\u{1}\(src)") {
            guard shown == nil, files != nil else { return }
            if case .success(let loaded) = await ImageLoader.shared.load(src, files: files, size: .thumbnail),
                !Task.isCancelled
            {
                image = loaded
            }
        }
        .fullScreenCover(item: $viewing) { image in
            ImageViewer(image: image, src: src, title: image.fileName)
        }
        .fullScreenCover(item: $page) { ArtifactViewer(page: $0) }
    }

    private func open(_ thumbnail: LoadedImage) async {
        if case .svg(let text) = thumbnail.content {
            page = ArtifactPage(kind: .svg, title: thumbnail.fileName, code: text)
            return
        }
        if case .success(let full) = await ImageLoader.shared.load(src, files: files, size: .block) {
            viewing = full
        }
    }
}

extension LoadedImage: Identifiable {
    var id: ObjectIdentifier { ObjectIdentifier(self) }
}

// MARK: - Viewer

/// Full screen: the image fitted, pinch or double-tap to zoom, Share.
struct ImageViewer: View {
    let image: LoadedImage
    let src: String
    let title: String

    @Environment(\.dismiss) private var dismiss
    @Environment(\.transcriptImageFiles) private var files
    @State private var sharp: UIImage?
    @State private var shareURL: URL?

    var body: some View {
        NavigationStack {
            ZoomableImageView(image: sharp ?? image.bitmap ?? UIImage())
                .ignoresSafeArea()
                .background(Color.black.ignoresSafeArea())
                .accessibilityIdentifier("image-viewer")
                .navigationTitle(title)
                .navigationBarTitleDisplayMode(.inline)
                .toolbarBackground(.visible, for: .navigationBar)
                .toolbar {
                    ToolbarItem(placement: .topBarLeading) {
                        Button("Close", systemImage: "xmark") { dismiss() }
                            .accessibilityIdentifier("image-viewer-close")
                    }
                    ToolbarItem(placement: .primaryAction) {
                        if let shareURL {
                            ShareLink(item: shareURL) {
                                Image(systemName: "square.and.arrow.up")
                            }
                            .accessibilityIdentifier("image-viewer-share")
                        }
                    }
                }
        }
        .preferredColorScheme(.dark)
        .task {
            shareURL = Self.shareFile(image)
            // The block decoded at most `ImageLoader.Size.block`; zooming in
            // reads a sharper one when the image is bigger than that.
            let longest = max(image.pixelSize.width, image.pixelSize.height)
            guard !image.animated, longest > ImageLoader.Size.block.maxPixel else { return }
            if case .success(let full) = await ImageLoader.shared.load(src, files: files, size: .viewer) {
                sharp = full.bitmap
            }
        }
    }

    /// The bytes as a file named like the image, for Share.
    private static func shareFile(_ image: LoadedImage) -> URL? {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("shared-images", isDirectory: true)
        try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        var name = image.fileName.replacingOccurrences(of: "/", with: "-")
        if ImageSource.Format(path: name) == nil { name += ".\(image.format.fileExtension)" }
        let url = folder.appendingPathComponent(name)
        do {
            try image.data.write(to: url, options: .atomic)
            return url
        } catch {
            return nil
        }
    }
}

/// A UIScrollView zooming one image: fitted at rest, pinch from 1× to 8×,
/// double tap to zoom in at the tap and back.
struct ZoomableImageView: UIViewRepresentable {
    let image: UIImage

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeUIView(context: Context) -> ZoomScrollView {
        let scroll = ZoomScrollView()
        scroll.delegate = context.coordinator
        scroll.minimumZoomScale = 1
        scroll.maximumZoomScale = 8
        scroll.showsHorizontalScrollIndicator = false
        scroll.showsVerticalScrollIndicator = false
        scroll.contentInsetAdjustmentBehavior = .never
        scroll.backgroundColor = .black
        scroll.imageView.image = image
        if (image.images?.count ?? 0) > 1 { scroll.imageView.startAnimating() }
        let doubleTap = UITapGestureRecognizer(
            target: context.coordinator, action: #selector(Coordinator.doubleTapped(_:)))
        doubleTap.numberOfTapsRequired = 2
        scroll.addGestureRecognizer(doubleTap)
        return scroll
    }

    func updateUIView(_ scroll: ZoomScrollView, context: Context) {
        if scroll.imageView.image !== image {
            scroll.imageView.image = image
            if (image.images?.count ?? 0) > 1 { scroll.imageView.startAnimating() }
        }
    }

    /// Keeps its image fitted to its bounds while not zoomed.
    final class ZoomScrollView: UIScrollView {
        let imageView = UIImageView()
        private var fitted: CGSize = .zero

        override init(frame: CGRect) {
            super.init(frame: frame)
            imageView.contentMode = .scaleAspectFit
            addSubview(imageView)
        }

        required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

        override func layoutSubviews() {
            super.layoutSubviews()
            guard bounds.size != fitted, bounds.size != .zero else { return }
            fitted = bounds.size
            zoomScale = 1
            imageView.frame = CGRect(origin: .zero, size: bounds.size)
            contentSize = bounds.size
        }
    }

    @MainActor
    final class Coordinator: NSObject, UIScrollViewDelegate {
        func viewForZooming(in scrollView: UIScrollView) -> UIView? {
            (scrollView as? ZoomScrollView)?.imageView
        }

        @objc func doubleTapped(_ gesture: UITapGestureRecognizer) {
            guard let scroll = gesture.view as? ZoomScrollView else { return }
            if scroll.zoomScale > 1.01 {
                scroll.setZoomScale(1, animated: true)
                return
            }
            let point = gesture.location(in: scroll.imageView)
            let scale: CGFloat = 3
            let size = CGSize(width: scroll.bounds.width / scale, height: scroll.bounds.height / scale)
            scroll.zoom(
                to: CGRect(
                    x: point.x - size.width / 2, y: point.y - size.height / 2,
                    width: size.width, height: size.height),
                animated: true)
        }
    }
}
