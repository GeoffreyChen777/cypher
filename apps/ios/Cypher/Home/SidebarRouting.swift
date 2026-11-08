// Session rows that push on the phone and select on the iPad sidebar. Home's
// list is the same view in both layouts; the split layout sets
// `sidebarRouting` on it, so a row opens its route in the detail column (and
// shows as selected) instead of pushing onto the sidebar's stack.

import SwiftUI

/// The iPad sidebar's selection: the route the detail column shows, and how
/// to open another one there.
struct SidebarRouting {
    var selected: Route?
    var open: (Route) -> Void
}

extension EnvironmentValues {
    @Entry var sidebarRouting: SidebarRouting? = nil
    /// The split layout's column visibility: a session's inspector hides the
    /// sidebar when the chat would be too narrow beside it. nil on the phone.
    @Entry var splitColumns: Binding<NavigationSplitViewVisibility>? = nil
}

extension View {
    /// A detail-column title clear of the sidebar button, which leads that
    /// bar while the sidebar is hidden.
    func clearsHiddenSidebarButton(_ columns: Binding<NavigationSplitViewVisibility>?) -> some View {
        padding(.leading, columns?.wrappedValue == .detailOnly ? 12 : 0)
    }
}

/// A list row for a route, in the grouped-row paint: a NavigationLink in a
/// stack, a selectable button in the iPad sidebar.
struct RouteRow<Label: View>: View {
    @Environment(\.sidebarRouting) private var routing
    let route: Route
    @ViewBuilder var label: Label

    var body: some View {
        if let routing {
            let selected = routing.selected == route
            Button {
                routing.open(route)
            } label: {
                label.contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            // The pointer lights the row it's over, as the selection will.
            .hoverEffect(.highlight)
            .listRowBackground(selected ? Theme.surfaceRaised : Theme.groupedRow)
            .listRowSeparatorTint(Theme.border)
            .accessibilityAddTraits(selected ? .isSelected : [])
        } else {
            NavigationLink(value: route) { label }
                .groupedRowStyle()
        }
    }
}
