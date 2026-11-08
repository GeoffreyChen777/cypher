// A stack showing a route path whose first route is its root: the iPad split
// layout's detail column (the sidebar's selection, then what it pushed) and
// a session window. The session inspector sits beside the stack.
//
// The inspector's own NavigationStack (Files drills into folders) must not
// be nested in another navigation container — inside one it pushes onto
// that container's stack instead. So a split view hosts it outside the
// whole split view (`inspectorHost`), and a window outside its one stack.

import SwiftUI

/// The screen a route opens.
struct RouteDestination: View {
    let route: Route
    @Binding var path: [Route]

    var body: some View {
        switch route {
        case .space(let id): SpaceView(spaceId: id, path: $path)
        case .chat(let id): SessionView(chatId: id, path: $path).id(id)
        case .newSession(let spaceId): NewSessionView(spaceId: spaceId, path: $path)
        case .quickChat(let deviceId):
            NewSessionView(spaceId: "", path: $path, quickDeviceId: deviceId)
        }
    }
}

struct DetailColumn<Placeholder: View>: View {
    /// The whole path: `path.first` is the root, the rest is pushed.
    @Binding var path: [Route]
    /// The split view's columns, for the inspector to make room; nil in a
    /// window of its own.
    var columns: Binding<NavigationSplitViewVisibility>? = nil
    var probe: NavigationProbe? = nil
    /// The inspector, when the enclosing split view presents it; nil
    /// presents one beside this stack.
    var inspectorHost: DetailInspector? = nil
    /// Shown while the path is empty.
    @ViewBuilder var placeholder: Placeholder

    @State private var ownInspector = DetailInspector()

    var body: some View {
        NavigationStack(path: pushed) {
            root
                .navigationDestination(for: Route.self) { RouteDestination(route: $0, path: $path) }
                .background {
                    if let probe { NavigationProbeView(probe: probe) }
                }
        }
        .environment(\.splitColumns, columns)
        .environment(\.detailInspector, inspectorHost ?? ownInspector)
        .presentsDetailInspector(inspectorHost == nil ? ownInspector : nil)
    }

    @ViewBuilder private var root: some View {
        if let root = path.first {
            RouteDestination(route: root, path: $path).id(root)
        } else {
            placeholder
        }
    }

    /// Everything past the root.
    private var pushed: Binding<[Route]> {
        Binding(
            get: { Array(path.dropFirst()) },
            set: { path = Array(path.prefix(1)) + $0 })
    }
}

extension View {
    /// Shows `host`'s panel as this view's inspector; nil shows none.
    func presentsDetailInspector(_ host: DetailInspector?) -> some View {
        inspector(
            isPresented: Binding(
                get: { host?.shown ?? false },
                set: { if !$0 { host?.hide() } })
        ) {
            host?.content
        }
    }
}
