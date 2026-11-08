// Account › Paired devices: join this iPhone to the account's device trust
// group with the code a paired Mac or PC shows (Settings › Devices › Add a
// device there), and see which devices are paired. DeviceTrustStore does the
// work; docs/design/device-trust.md explains the protocol.

import SwiftUI

struct PairedDevicesView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var code = ""

    var body: some View {
        NavigationStack {
            Form {
                if let trust = model.config?.trust {
                    content(trust)
                } else {
                    Text("Pairing needs a signed-in account.")
                        .font(Theme.sans(13)).foregroundStyle(Theme.textMuted)
                }
            }
            .navigationTitle("Paired devices")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
            }
            .task { await model.config?.trust?.sync() }
        }
    }

    @ViewBuilder
    private func content(_ trust: DeviceTrustStore) -> some View {
        Section {
            LabeledContent("This iPhone", value: Self.stateLabel(trust.phase))
            LabeledContent("Key", value: trust.fingerprint)
                .font(Theme.mono(12))
            if trust.conflict {
                Text(
                    "The server's list of paired devices disagrees with this iPhone's copy. The iPhone keeps its own copy. If you didn't change your devices, someone may be tampering with the server."
                )
                .font(Theme.sans(12)).foregroundStyle(Theme.textMuted)
            }
        } footer: {
            Text(
                "Paired devices prove who they are to each other with their own keys, so a stolen sign-in or a compromised server can't control them."
            )
        }

        if trust.phase == .paired {
            Section("Paired devices") {
                ForEach(trust.members, id: \.id) { member in
                    LabeledContent(member.id == trust.deviceId ? "\(member.name) (this iPhone)" : member.name) {
                        Text(member.fingerprint).font(Theme.mono(11))
                    }
                }
            }
            Section {
                LabeledContent("Only paired devices", value: trust.requirePaired ? "On" : "Off")
            } footer: {
                Text("Change this or add devices under Settings › Devices on a paired Mac or PC.")
            }
        } else if trust.joining {
            Section {
                HStack(spacing: 10) {
                    ProgressView()
                    Text("Waiting for approval on your Mac or PC.")
                }
                Text("It will show this iPhone's key: \(trust.fingerprint).")
                    .font(Theme.sans(12)).foregroundStyle(Theme.textMuted)
                Button("Cancel", role: .cancel) { trust.cancelJoin() }
            }
        } else {
            Section {
                TextField("XXXX-XXXX-XXXX-XXXX-XXXX", text: $code)
                    .font(Theme.mono(15))
                    .textInputAutocapitalization(.characters)
                    .autocorrectionDisabled()
                Button("Pair") {
                    trust.join(code: code)
                    code = ""
                }
                .disabled(code.trimmingCharacters(in: .whitespaces).isEmpty)
            } header: {
                Text("Pair this iPhone")
            } footer: {
                Text("On a paired Mac or PC, open Settings › Devices and click Add a device. Enter the code it shows.")
            }
        }

        if let error = trust.error {
            Section {
                Text(error).font(Theme.sans(12)).foregroundStyle(Theme.textMuted)
            }
        }
    }

    static func stateLabel(_ phase: DeviceTrustStore.Phase) -> String {
        switch phase {
        case .paired: return "Paired"
        case .unpaired: return "Not paired"
        case .removed: return "Removed — pair again"
        case .unknown: return "Checking…"
        }
    }
}
