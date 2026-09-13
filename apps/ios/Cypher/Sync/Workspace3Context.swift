import Foundation

/// One account control connection, shared by sidebar, sessions and attachments.
@MainActor
final class Workspace3Context {
    let journal: Workspace3Journal
    private(set) var client: Workspace3Client!
    private(set) var rpc: Workspace3RPC!
    private var observers: [UUID: (Workspace3Event?) -> Void] = [:]
    private var retired = false
    private var wanted: [UUID: String] = [:]

    init(config: AppConfig, journal: Workspace3Journal? = nil) throws {
        let scope = try config.workspace3Scope()
        self.journal = try journal ?? Workspace3Journal(url: Workspace3Journal.defaultURL(scope: scope), scope: scope)
        client = Workspace3Client(journal: self.journal, request: { [weak config] in
            guard let config, config.isActive else { throw RelayError.notConnected }
            return try await config.workspace3Request()
        }, active: { [weak config] in config?.isActive == true }, event: { [weak self] event in
            guard let self, !self.retired else { return }
            self.rpc?.event(event)
            for listener in Array(self.observers.values) { listener(event) }
        })
        rpc = Workspace3RPC(client: client, active: { [weak config] in config?.isActive == true })
        client.onChange = { [weak self] in
            guard let self, !self.retired else { return }
            for listener in Array(self.observers.values) { listener(nil) }
        }
    }
    func observe(_ listener: @escaping (Workspace3Event?) -> Void) -> UUID {
        let id = UUID(); observers[id] = listener; return id
    }
    func removeObserver(_ id: UUID) { observers.removeValue(forKey: id) }
    func want(_ chat: String) throws -> UUID {
        guard !retired, Workspace3Wire.id(chat), Set(wanted.values).union([chat]).count <= 8 else { try Workspace3Wire.fail("read_interest_capacity") }
        let id = UUID(); wanted[id] = chat
        try client.watch(Array(Set(wanted.values)))
        return id
    }
    func release(_ id: UUID) {
        wanted.removeValue(forKey: id)
        if !retired { try? client.watch(Array(Set(wanted.values))) }
    }
    func retire() {
        guard !retired else { return }
        retired = true; rpc.retire(); client.retire(); observers.removeAll(); wanted.removeAll()
        let client = client!
        Task { await client.stop() }
    }
}
