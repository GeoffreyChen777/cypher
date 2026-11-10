// How a Project page leaves the board for the app's own screens: the
// presenter (the workspace screen) supplies it, leaving the board and
// pushing the route. Nil where nothing can be opened: the page then offers
// no "Open project".

import SwiftUI

extension EnvironmentValues {
    /// Opens an app route (a project, `Route.space(id)`) outside the board.
    @Entry var openBotRoute: ((Route) -> Void)? = nil
}
