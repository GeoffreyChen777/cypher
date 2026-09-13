import Foundation

enum RelayError: LocalizedError {
    case notConnected, hostOffline, timeout
    case rpc(String)
    var errorDescription: String? {
        switch self {
        case .notConnected: return "Not connected to the device"
        case .hostOffline: return "The device is offline"
        case .timeout: return "The device didn't respond"
        case .rpc(let message): return message
        }
    }
}
