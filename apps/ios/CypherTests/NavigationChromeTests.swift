import XCTest
import SwiftUI
@testable import Cypher

@MainActor
private final class NavigationDriver: ObservableObject {
    @Published var path: [Route] = []
}

private struct NavigationChromeFixture: View {
    @ObservedObject var driver: NavigationDriver
    let model: AppModel

    var body: some View {
        NavigationStack(path: $driver.path) {
            Text("Projects")
                .navigationTitle("Projects")
                .navigationBarTitleDisplayMode(.inline)
                .toolbarBackground(.hidden, for: .navigationBar)
                .navigationDestination(for: Route.self) { route in
                    switch route {
                    case .space(let id): SpaceView(spaceId: id, path: $driver.path)
                    case .chat(let id): SessionView(chatId: id, path: $driver.path).id(id)
                    case .newSession(let id): NewSessionView(spaceId: id, path: $driver.path)
                    }
                }
        }
        .environment(model)
    }
}

@MainActor
final class NavigationChromeTests: XCTestCase {
    private func navigationController(in controller: UIViewController) -> UINavigationController? {
        if let nav = controller as? UINavigationController { return nav }
        for child in controller.children {
            if let nav = navigationController(in: child) { return nav }
        }
        return nil
    }

    private func settle(_ condition: () -> Bool) async throws {
        for _ in 0..<75 {
            if condition() { return }
            try await Task.sleep(for: .milliseconds(40))
        }
        XCTFail("Navigation did not settle")
        throw NSError(domain: "NavigationChromeTests", code: 1)
    }

    func testSessionTitlesStayOutOfBackButtonGroupAcrossAnimatedPushAndPop() async throws {
        // No restore/login/network. Use the same views as production, backed
        // by an isolated in-memory demo and an explicitly driven navigation path.
        let model = AppModel()
        model.enterDemoMode()
        let driver = NavigationDriver()
        let host = UIHostingController(rootView: NavigationChromeFixture(driver: driver, model: model))
        let scene = try XCTUnwrap(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
        let previousKeyWindow = scene.windows.first(where: \.isKeyWindow)
        let window = UIWindow(windowScene: scene)
        window.rootViewController = host
        window.makeKeyAndVisible()
        defer {
            window.isHidden = true
            window.rootViewController = nil
            previousKeyWindow?.makeKey()
        }
        try await settle { self.navigationController(in: host) != nil }
        let nav = try XCTUnwrap(navigationController(in: host))
        driver.path = [.space("space-cypher")]
        try await settle { nav.viewControllers.count == 2 && nav.transitionCoordinator == nil }
        let project = try XCTUnwrap(nav.topViewController)

        for destination: Route in [.chat("chat-tabs"), .newSession(spaceId: "space-cypher")] {
            withAnimation { driver.path.append(destination) }
            try await settle { nav.viewControllers.count == 3 && nav.transitionCoordinator == nil }
            let item = try XCTUnwrap(nav.navigationBar.topItem)
            XCTAssertEqual(item.leftBarButtonItems?.count ?? 0, 0,
                           "A static session title must not be a wide leading bar button")
            XCTAssertTrue(item.leadingItemGroups.flatMap(\.barButtonItems).isEmpty,
                          "iOS 26 toolbar groups must not contain the static session title")
            XCTAssertNotNil(item.titleView, "Use the native title-view slot")
            XCTAssertFalse(item.hidesBackButton, "Keep native Back rather than a replacement button")
            XCTAssertNotNil(nav.interactivePopGestureRecognizer)

            withAnimation { driver.path.removeLast() }
            try await settle { nav.viewControllers.count == 2 && nav.transitionCoordinator == nil }
            XCTAssertTrue(nav.topViewController === project)
            let restored = try XCTUnwrap(nav.navigationBar.topItem)
            XCTAssertEqual(restored.leftBarButtonItems?.count ?? 0, 0)
            XCTAssertTrue(restored.leadingItemGroups.flatMap(\.barButtonItems).isEmpty)
            XCTAssertFalse(restored.hidesBackButton)
        }
    }
}
