import Foundation

/// Existing call-site facade; all requests now use the account WorkspaceHub.
/// This owns no socket and has no automatic call retry.
actor WorkspaceRemote {
    private let deviceId: String
    private let config: AppConfig
    private let owner = UUID().uuidString
    private var closed = false
    private var tasks: [UUID: Task<Data, Error>] = [:]
    private var admitted = 0
    init(deviceId: String, config: AppConfig) { self.deviceId = deviceId; self.config = config }
    func close() async {
        closed = true
        for task in tasks.values { task.cancel() }
        await config.cancelWorkspaceCalls(owner: owner)
    }
    func call<Response: Decodable>(method: String, params: [String: Any], timeoutSeconds: UInt64 = 10) async throws -> Response {
        guard !closed, config.isActive else { throw RelayError.notConnected }
        guard admitted < 8 else { try Workspace3Wire.fail("rpc_capacity") }
        admitted += 1
        defer { admitted -= 1 }
        let value = try Workspace3RPCCodec.foundation(params)
        let context = try await config.workspaceContext()
        guard !closed, config.isActive else { throw RelayError.notConnected }
        let id = UUID(), owner = self.owner, target = deviceId
        let task = Task { try await context.rpc.call(owner: owner, target: target, method: method, params: value, timeout: timeoutSeconds) }
        tasks[id] = task
        defer { tasks.removeValue(forKey: id) }
        let result = try await withTaskCancellationHandler {
            try await task.value
        } onCancel: { task.cancel() }
        guard !closed, config.isActive else { throw RelayError.notConnected }
        return try JSONDecoder().decode(Response.self, from: result)
    }
}
