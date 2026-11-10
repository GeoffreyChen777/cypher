// A workspace's board: its own NavigationStack, presented over the
// workspace chat, whose root is the page it opened on and which pushes
// `BotPage`s. `BotPageView` maps each page to its screen; every page group
// lives in a file of its own (Board/, Connections/, Memory/), so this file
// only changes when a page is added.

import SwiftUI

struct BotBoard: View {
    let botId: String
    var page: BotPage = .overview
    @State private var path: [BotPage] = []

    var body: some View {
        NavigationStack(path: $path) {
            BotPageView(botId: botId, page: page)
                .navigationDestination(for: BotPage.self) { BotPageView(botId: botId, page: $0) }
        }
    }
}

/// The screen for one board page.
struct BotPageView: View {
    let botId: String
    let page: BotPage

    var body: some View {
        switch page {
        case .overview: BotOverviewPage(botId: botId)
        case .needsYou: NeedsYouPage(botId: botId)
        case .approval(let id): ApprovalPage(botId: botId, approvalId: id)
        case .asking(let chatId, let workId): AskingPage(botId: botId, chatId: chatId, workId: workId)
        case .work: WorkListPage(botId: botId)
        case .workItem(let id): WorkItemPage(botId: botId, workId: id)
        case .routines: RoutineListPage(botId: botId)
        case .routine(let id): RoutinePage(botId: botId, routineId: id)
        case .routineForm(let id): RoutineFormPage(botId: botId, routineId: id)
        case .projects: BotProjectListPage(botId: botId)
        case .project(let id): BotProjectPage(botId: botId, spaceId: id)
        case .activity: BotActivityPage(botId: botId)
        case .connections: ConnectionListPage(botId: botId)
        case .connection(let id): ConnectionPage(botId: botId, connectionId: id)
        case .exchange(let id): ExchangePage(botId: botId, exchangeId: id)
        case .connect: ConnectPage(botId: botId)
        case .memory(let scope): MemoryPage(botId: botId, scope: scope)
        case .memoryFile(let scope, let path): MemoryFilePage(botId: botId, scope: scope, path: path)
        }
    }
}

/// A stub page's body until its track builds it: the title and a plain
/// list of what it will show. (Remove once no page uses it.)
struct BotStubPage<Content: View>: View {
    let title: String
    @ViewBuilder var content: Content

    var body: some View {
        List {
            content
        }
        .listStyle(.insetGrouped)
        .scrollContentBackground(.hidden)
        .background(Theme.surface.ignoresSafeArea())
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.inline)
    }
}
