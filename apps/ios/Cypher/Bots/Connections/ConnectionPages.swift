// A workspace's Connections: the connected workspaces, invites and recent
// questions; one connection (reply mode, scope, its questions, revoke); one
// question (a waiting one opens as its review); and the connect page
// (create an invite code, or paste one). Stubs.

import SwiftUI

struct ConnectionListPage: View {
    @Environment(AppModel.self) private var model
    let botId: String

    var body: some View {
        BotStubPage(title: "Connections") {
            Section {
                ForEach(model.data?.connections(of: botId) ?? []) { connection in
                    NavigationLink(value: BotPage.connection(connection.id)) {
                        HStack(spacing: 10) {
                            PeerAvatar(peer: connection.peer)
                            LabeledContent(connection.peer.name, value: connection.replyMode.label)
                        }
                    }
                }
                NavigationLink("Connect", value: BotPage.connect)
            }
            Section {
                ForEach(model.data?.exchanges(of: botId) ?? []) { exchange in
                    NavigationLink(exchange.question, value: BotPage.exchange(exchange.id))
                }
            }
        }
    }
}

struct ConnectionPage: View {
    @Environment(AppModel.self) private var model
    let botId: String
    let connectionId: String

    var body: some View {
        let connection = model.data?.connection(id: connectionId)
        BotStubPage(title: connection?.peer.name ?? "Connection") {
            if let connection {
                BotFact(label: "Owner", value: connection.peer.ownerName)
                BotFact(label: "Replies", value: connection.replyMode.label)
            }
        }
    }
}

struct ExchangePage: View {
    @Environment(AppModel.self) private var model
    let botId: String
    let exchangeId: String

    var body: some View {
        let exchange = model.data?.exchange(id: exchangeId)
        BotStubPage(title: "Question") {
            if let exchange {
                BotFact(label: "Question", value: exchange.question)
                if let answer = exchange.answer { BotFact(label: "Answer", value: answer) }
                BotFact(status: exchange.status)
            }
        }
    }
}

struct ConnectPage: View {
    let botId: String

    var body: some View {
        BotStubPage(title: "Connect") {
            Text("Create an invite code, or paste one.")
        }
    }
}
