import SwiftUI

struct PiCatalogNotice: View {
    let catalog: RemotePiCatalog
    let deviceName: String
    let retry: () -> Void

    var body: some View {
        if catalog.loading || catalog.error != nil {
            HStack(alignment: .top, spacing: 10) {
                if catalog.loading {
                    ProgressView().controlSize(.small)
                }
                Text(catalog.loading
                     ? "Loading Pi models from \(deviceName)…"
                     : "\(deviceName): \(catalog.error?.message ?? "")")
                    .font(Theme.sans(12))
                    .foregroundStyle(Theme.textMuted)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if !catalog.loading {
                    Button("Retry", action: retry).font(Theme.sans(12, weight: .medium))
                }
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 8)
        }
    }
}
