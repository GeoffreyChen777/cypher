import SwiftUI

enum SessionConnectionPhase: Equatable {
    case connecting
    case ready
    case catalogFailure(PiCatalogError)
    case missingModel

    static let noticeDelay: TimeInterval = 15

    static func resolve(transportReady: Bool, needsCatalog: Bool, catalogMatches: Bool,
                        catalogLoading: Bool, catalogError: PiCatalogError?,
                        modelAvailable: Bool) -> Self {
        guard transportReady else { return .connecting }
        guard needsCatalog else { return .ready }
        guard catalogMatches, !catalogLoading else { return .connecting }
        if let catalogError { return .catalogFailure(catalogError) }
        return modelAvailable ? .ready : .missingModel
    }

    func summary(elapsed: TimeInterval) -> String? {
        switch self {
        case .ready: nil
        case .connecting: elapsed >= Self.noticeDelay ? "Connection timed out" : nil
        case .catalogFailure(.unavailable): "Couldn't load models"
        case .catalogFailure(.runtimeUnavailable): "Pi unavailable"
        case .catalogFailure(.noModels): "No models available"
        case .missingModel: "Select an available model"
        }
    }

    var detail: String {
        switch self {
        case .catalogFailure(let error): error.message
        case .connecting: "The connection is taking longer than expected. Automatic reconnection continues; you can also retry."
        case .missingModel: "Choose an available model below. Your previous selection hasn't been changed."
        case .ready: ""
        }
    }
}

/// One compact connection indicator for both the session transport and model
/// catalog. The parent reserves its row, so reconnecting cannot add paragraphs
/// above the input or move the transcript around.
struct SessionConnectionIndicator: View {
    let phase: SessionConnectionPhase
    let retryRevision: Int
    let retry: () -> Void
    @Environment(\.scenePhase) private var scenePhase
    @State private var began = Date()
    @State private var showDetails = false

    var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { context in
            HStack(spacing: 8) {
                if let summary = phase.summary(elapsed: context.date.timeIntervalSince(began)) {
                    Button {
                        showDetails = true
                    } label: {
                        Text(summary)
                            .font(Theme.sans(11))
                            .foregroundStyle(Theme.warning)
                            .lineLimit(1)
                    }
                    .buttonStyle(.plain)
                    if phase != .missingModel {
                        Button("Retry", action: retry)
                            .font(Theme.sans(11, weight: .medium))
                            .foregroundStyle(Theme.textMuted)
                    }
                } else if phase == .connecting {
                    ProgressView()
                        .controlSize(.mini)
                        .tint(Theme.textMuted)
                        .accessibilityLabel("Connecting")
                }
            }
        }
        .onChange(of: phase) { old, new in
            if new == .connecting, old != .connecting { began = Date() }
        }
        .onChange(of: retryRevision) { _, _ in began = Date() }
        .onChange(of: scenePhase) { _, new in
            if new == .active, phase == .connecting { began = Date() }
        }
        .alert("Connection details", isPresented: $showDetails) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(phase.detail)
        }
    }
}
