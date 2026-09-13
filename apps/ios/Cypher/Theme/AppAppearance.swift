import SwiftUI

/// Client-local only: appearance never changes synced workspace preferences.
enum AppAppearance: String, CaseIterable, Identifiable, Hashable {
    case system
    case light
    case dark

    static let storageKey = "appAppearance"
    var id: String { rawValue }

    init(storedValue: String) {
        self = Self(rawValue: storedValue) ?? .system
    }

    var label: String {
        switch self {
        case .system: "System"
        case .light: "Light"
        case .dark: "Dark"
        }
    }

    var colorScheme: ColorScheme? {
        switch self {
        case .system: nil
        case .light: .light
        case .dark: .dark
        }
    }
}

struct AppAppearanceModifier: ViewModifier {
    @AppStorage(AppAppearance.storageKey) private var storedAppearance = AppAppearance.system.rawValue

    func body(content: Content) -> some View {
        content.preferredColorScheme(AppAppearance(storedValue: storedAppearance).colorScheme)
    }
}

struct AppearancePicker: View {
    @AppStorage(AppAppearance.storageKey) private var storedAppearance = AppAppearance.system.rawValue

    var body: some View {
        Picker("Appearance", selection: Binding(
            get: { AppAppearance(storedValue: storedAppearance) },
            set: { storedAppearance = $0.rawValue }
        )) {
            ForEach(AppAppearance.allCases) { appearance in
                Text(appearance.label).tag(appearance)
            }
        }
        .pickerStyle(.menu)
    }
}
