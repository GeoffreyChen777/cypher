// One memory store (its files, held writes and change log, live through
// `watchMemory`) and one file of it, with edits through `editMemory`. Stubs.

import SwiftUI

struct MemoryPage: View {
    @Environment(AppModel.self) private var model
    let botId: String
    let scope: MemoryScope
    @State private var view: MemoryView?
    @State private var error: String?

    var body: some View {
        BotStubPage(title: "Memory") {
            if let error { Text(error).foregroundStyle(Theme.danger) }
            ForEach(view?.files ?? []) { file in
                NavigationLink(file.path, value: BotPage.memoryFile(scope, path: file.path))
            }
        }
        .task(id: scope) {
            guard let data = model.data else { return }
            do {
                for try await next in data.watchMemory(scope: scope) { view = next }
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

struct MemoryFilePage: View {
    let botId: String
    let scope: MemoryScope
    let path: String

    var body: some View {
        BotStubPage(title: path) {
            Text(path)
        }
    }
}
