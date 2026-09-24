import XCTest

extension XCUIElement {
    /// The transcript exists (hidden behind its skeleton) from the first frame
    /// and is revealed only once it has settled at the bottom — so "visible"
    /// assertions must wait for hittability, not just existence.
    func waitForHittable(timeout: TimeInterval) -> Bool {
        let expectation = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "exists == true AND hittable == true"), object: self)
        return XCTWaiter.wait(for: [expectation], timeout: timeout) == .completed
    }
}
