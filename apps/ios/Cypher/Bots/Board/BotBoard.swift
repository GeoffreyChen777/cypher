// A workspace's board: its own NavigationStack, presented over the
// workspace chat (a sheet on iPhone, an inspector column on iPad), whose
// root is the page it opened on and which pushes `BotPage`s. `BotPageView`
// maps each page to its screen; every page group lives in a file of its own
// (Board/, Connections/, Memory/), so this file only changes when a page is
// added. Pages open sessions through `\.openBotChat` (the presenter leaves
// the board and shows the chat); a hidden thread the board names opens
// read-only inside it (`BotThreadLink`).

import SwiftUI

extension EnvironmentValues {
    /// `openBotChat`, doing nothing where nothing can be opened.
    var botOpenChat: (String) -> Void { openBotChat ?? { _ in } }
}

/// A hidden thread (an orchestration's, a routine's, a connection's) pushed
/// onto the board, read-only.
struct BotThreadLink: Hashable {
    let chatId: String
}

struct BotBoard: View {
    let botId: String
    var initial: BotPage = .overview
    var openChat: (String) -> Void = { _ in }
    /// The toolbar's ×; nil dismisses (a sheet).
    var close: (() -> Void)?

    @Environment(\.dismiss) private var dismiss
    @Environment(\.boardPageShown) private var pageShown
    @State private var path = NavigationPath()

    init(
        botId: String, initial: BotPage = .overview, openChat: @escaping (String) -> Void = { _ in },
        close: (() -> Void)? = nil
    ) {
        self.botId = botId
        self.initial = initial
        self.openChat = openChat
        self.close = close
    }

    /// The stub presenter's call.
    init(botId: String, page: BotPage) {
        self.init(botId: botId, initial: page)
    }

    var body: some View {
        let close = close ?? { dismiss() }
        NavigationStack(path: $path) {
            BotPageView(botId: botId, page: initial)
                .toolbar {
                    ToolbarItem(placement: .topBarTrailing) {
                        Button("Close", systemImage: "xmark", action: close)
                            .accessibilityIdentifier("board-close")
                    }
                }
                .navigationDestination(for: BotPage.self) { page in
                    // The phone's sheet grows for a page that needs the room.
                    BotPageView(botId: botId, page: page).onAppear { pageShown?(page) }
                }
                .navigationDestination(for: BotThreadLink.self) { BotThreadPage(chatId: $0.chatId) }
        }
        .environment(\.openBotChat, openChat)
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

/// A board page's list: inset-grouped on the app's surface, its title inline.
struct BotPageList<Content: View>: View {
    let title: String
    @ViewBuilder var content: Content

    var body: some View {
        List {
            content
        }
        .botPageStyle(title: title)
    }
}

extension View {
    /// Every board page's list: inset-grouped on the app's surface, the
    /// title inline (the kind of page; the header names the item), and the
    /// first section right under the bar, as on a Settings page.
    func botPageStyle(title: String) -> some View {
        listStyle(.insetGrouped)
            .contentMargins(.top, BotPageMetrics.topMargin, for: .scrollContent)
            .scrollContentBackground(.hidden)
            .background(Theme.surface.ignoresSafeArea())
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
    }
}

enum BotPageMetrics {
    /// Above a page's first section (the list's own default is ~20pt more).
    static let topMargin: CGFloat = 4
}

/// A hidden thread's transcript, read-only (work.rs `render_thread`): what
/// the workspace did there, under what owns it.
struct BotThreadPage: View {
    @Environment(AppModel.self) private var model
    let chatId: String
    @State private var scroll = ScrollState()

    var body: some View {
        Group {
            if let chat = model.chat(id: chatId), let store = model.sessionStore(for: chat) {
                TranscriptView(store: store, chatId: chatId, scroll: scroll)
                    .id(chatId)
                    .onAppear { model.sessionOnScreen(chatId) }
                    .onDisappear {
                        model.sessionOffScreen(chatId)
                        model.releaseSessionStore(chatId: chatId)
                    }
            } else {
                ContentUnavailableView(
                    "Not on this device yet", systemImage: "bubble.left.and.text.bubble.right",
                    description: Text("The session syncs from the workspace's device."))
            }
        }
        .background(Theme.bg.ignoresSafeArea())
        .environment(\.commentDrafts, nil)
        .environment(\.transcriptSelectionActions, nil)
        .environment(\.transcriptMessageActions, nil)
        .environment(\.openWorkspace, nil)
        .environment(\.fileReferenceContext, nil)
        .environment(\.fileReferenceSnippet, nil)
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.inline)
    }

    /// What owns the thread: its work, its routine, the connection asking.
    private var title: String {
        guard let chat = model.chat(id: chatId) else { return "Session" }
        switch chat.bot?.thread {
        case .work(let id)?: return model.data?.work(id: id)?.title ?? chat.displayTitle
        case .routine(let id)?, .routineSession(let id)?:
            return model.data?.routine(id: id)?.name ?? chat.displayTitle
        case .peer(let id)?:
            return model.data?.connection(id: id).map { "Questions from \($0.peer.name)" } ?? chat.displayTitle
        default: return chat.displayTitle
        }
    }
}
