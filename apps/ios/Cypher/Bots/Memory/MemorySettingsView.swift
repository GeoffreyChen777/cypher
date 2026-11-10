// Settings › Memory: every store (yours, each project's, each workspace's)
// with its counts, consolidation schedule and last pass. Stub.

import SwiftUI

struct MemorySettingsView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        List {
            ForEach(model.botRecords.memoryStores) { store in
                LabeledContent(store.scope.root, value: "\(store.files) files")
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle("Memory")
    }
}
