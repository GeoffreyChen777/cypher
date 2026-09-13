import SwiftUI

/// A code reader, not an editor. It shares Changes' offline renderer and never
/// gives the page a file lookup, command, or clipboard bridge.
struct WorkspaceSourceView: View {
    let path: String
    let partial: Bool
    let preview: WorkspaceTextPreview
    private let oversized: Bool
    @Environment(\.colorScheme) private var colorScheme
    @State private var wrap = false
    @State private var plain = false
    @State private var status: DiffRendererStatus = .loading
    @State private var hasRendered = false
    @State private var revision = 0

    init(path: String, text: String, partial: Bool) {
        self.path = path
        self.partial = partial
        preview = WorkspaceTextPreview(text)
        let lines = preview.text.split(separator: "\n", omittingEmptySubsequences: false).count
            - (preview.text.hasSuffix("\n") ? 1 : 0)
        oversized = preview.truncated || lines > 10_000
    }

    private var fallback: Bool { plain || oversized || status == .failed || status == .tooLarge }

    var body: some View {
        VStack(spacing: 0) {
            if partial || preview.truncated {
                notice("Preview truncated — showing loaded text", icon: "exclamationmark.triangle", warning: true)
            }
            if preview.text.isEmpty {
                ContentUnavailableView("Empty file", systemImage: "doc.text",
                    description: Text("This file has no text."))
            } else if fallback {
                notice(oversized ? "Large file · Plain text preview"
                       : plain ? "Plain text preview"
                       : "Highlighting unavailable · Plain text preview", icon: "doc.plaintext")
                WorkspaceCodeView(text: preview.text, readerStyle: true)
            } else {
                ZStack {
                    WorkspaceDiffWebView(path: path, patch: "", split: false, dark: colorScheme == .dark,
                        source: .init(text: preview.text, wrap: wrap)) { value in
                            status = value
                            if value == .rendered { hasRendered = true }
                        }
                        .id(revision)
                    if !hasRendered && status == .loading {
                        ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
                            .background(Theme.bg)
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.bg)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Menu {
                    if !fallback && !preview.text.isEmpty {
                        Toggle(isOn: $wrap) { Label("Wrap lines", systemImage: "arrow.turn.down.left") }
                    }
                    Button("Copy loaded text", systemImage: "doc.on.doc") {
                        UIPasteboard.general.string = preview.text
                    }
                    .disabled(preview.text.isEmpty)
                    if !oversized && !preview.text.isEmpty {
                        if fallback {
                            Button("Syntax highlighting", systemImage: "chevron.left.forwardslash.chevron.right") {
                                plain = false; status = .loading; hasRendered = false; revision += 1
                            }
                        } else {
                            Button("Plain text preview", systemImage: "doc.plaintext") { plain = true }
                        }
                    }
                } label: {
                    Image(systemName: "textformat")
                        .font(.system(size: 18, weight: .regular))
                        .frame(width: 22, height: 22)
                }
                .accessibilityLabel("Code options")
                .accessibilityIdentifier("workspace-code-options")
            }
        }
    }

    private func notice(_ title: String, icon: String, warning: Bool = false) -> some View {
        HStack(spacing: 10) {
            Image(systemName: icon).frame(width: 18, height: 18)
            Text(title).font(Theme.sans(11)).frame(maxWidth: .infinity, alignment: .leading)
        }
        .foregroundStyle(warning ? Theme.warning : Theme.textMuted)
        .padding(.horizontal, 16).padding(.vertical, 10)
        .frame(minHeight: 44)
        .background(Theme.surface)
    }
}
