import AppKit
import XCTest

@MainActor
final class RestoreTests: XCTestCase {
    private var app: XCUIApplication!
    private var fixtures: URL!
    private var evidence: URL!

    override func setUpWithError() throws {
        continueAfterFailure = false
        let environment = ProcessInfo.processInfo.environment
        let path = try XCTUnwrap(environment["ZFSE_GUI_APP"])
        fixtures = URL(fileURLWithPath: try XCTUnwrap(environment["ZFSE_GUI_FIXTURES"]))
        evidence = URL(fileURLWithPath: try XCTUnwrap(environment["ZFSE_GUI_EVIDENCE"]))
        app = XCUIApplication(url: URL(fileURLWithPath: path))
        app.launch()
        XCTAssertTrue(app.windows.firstMatch.waitForExistence(timeout: 20))
    }

    override func tearDown() { app?.terminate() }

    private func element(_ query: XCUIElementQuery, containing value: String) -> XCUIElement {
        let predicate = NSPredicate(format: "value CONTAINS %@ OR label CONTAINS %@ OR title CONTAINS %@", value, value, value)
        return query.matching(predicate).firstMatch
    }

    private func click(_ item: XCUIElement, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertTrue(item.waitForExistence(timeout: 15), "Missing \(item)", file: file, line: line)
        item.click()
    }

    private func goTo(_ path: URL) {
        app.typeKey("g", modifierFlags: [.command, .shift])
        let field = app.textFields["PathTextField"]
        XCTAssertTrue(field.waitForExistence(timeout: 10))
        field.click()
        field.typeKey("a", modifierFlags: .command)
        field.typeText(path.path)
        field.typeKey(.return, modifierFlags: [])
        let gone = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: field)
        XCTAssertEqual(XCTWaiter.wait(for: [gone], timeout: 10), .completed)
    }

    private func open(_ filename: String) {
        app.typeKey("o", modifierFlags: .command)
        XCTAssertTrue(app.buttons["CancelButton"].waitForExistence(timeout: 10))
        goTo(fixtures.appendingPathComponent(filename))
        click(app.buttons["OKButton"])
    }

    private func restore(to directory: URL) {
        click(app.toolbars.buttons.matching(identifier: "Restore…").firstMatch)
        XCTAssertTrue(app.buttons["CancelButton"].waitForExistence(timeout: 10))
        goTo(directory)
        click(app.buttons["OKButton"])
    }

    private func capture(_ name: String) {
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "zfs-" + name
        screenshot.lifetime = .keepAlways
        add(screenshot)
    }

    func testEncryptedRestoreAndOccupiedDestination() throws {
        open("encrypted-raw-s1.zfs")
        let field = app.secureTextFields.firstMatch
        click(field)
        field.typeText("incorrect-public-test-key\n")
        XCTAssertTrue(element(app.staticTexts, containing: "unlock this backup").waitForExistence(timeout: 10))
        capture("wrong-key")
        click(app.buttons["OK"])
        click(field)
        field.typeText("zfs-send-fixture-passphrase\n")
        click(element(app.outlines.staticTexts, containing: "docs"))
        click(app.buttons["Open Folder"])
        click(element(app.outlines.staticTexts, containing: "hello.txt"))
        let recovered = evidence.appendingPathComponent("restored")
        restore(to: recovered)
        XCTAssertTrue(app.buttons["Show in Finder"].waitForExistence(timeout: 15))
        capture("encrypted-restored")

        let occupied = evidence.appendingPathComponent("occupied")
        restore(to: occupied)
        click(app.buttons["Replace"])
        XCTAssertTrue(element(app.staticTexts, containing: "already there").waitForExistence(timeout: 10))
        capture("same-name-refused")
    }

    func testStripeRestore() throws {
        let env = ProcessInfo.processInfo.environment
        let directory = URL(fileURLWithPath: try XCTUnwrap(env["ZFSE_GUI_STRIPE_DIRECTORY"]))
        let dataset = try XCTUnwrap(env["ZFSE_GUI_STRIPE_DATASET"])
        let left = directory.appendingPathComponent("left.img")

        app.typeKey("o", modifierFlags: .command)
        XCTAssertTrue(app.buttons["CancelButton"].waitForExistence(timeout: 10))
        goTo(left)
        let right = element(app.descendants(matching: .any), containing: "right.img")
        XCTAssertTrue(right.waitForExistence(timeout: 10))
        XCUIElement.perform(withKeyModifiers: .command) { right.click() }
        capture("stripe-members")
        click(app.buttons["OKButton"])
        let button = app.buttons[dataset]
        if button.exists {
            click(button)
        } else {
            // Some macOS releases expose SwiftUI list buttons as table rows.
            // The button sits at the leading edge of the row.
            let row = element(app.tableRows, containing: dataset)
            XCTAssertTrue(row.waitForExistence(timeout: 15))
            row.coordinate(withNormalizedOffset: CGVector(dx: 0.06, dy: 0.5)).click()
        }
        let across = element(app.outlines.staticTexts, containing: "across.bin")
        click(across)
        capture("stripe-browser")

        app.typeKey("o", modifierFlags: .command)
        XCTAssertTrue(app.buttons["CancelButton"].waitForExistence(timeout: 10))
        goTo(left)
        click(app.buttons["OKButton"])
        XCTAssertTrue(element(app.staticTexts, containing: "Open all members").waitForExistence(timeout: 15))
        capture("stripe-incomplete")
        click(app.buttons["OK"])
        click(across)
        restore(to: evidence.appendingPathComponent("restored"))
        XCTAssertTrue(app.buttons["Show in Finder"].waitForExistence(timeout: 60))
        capture("stripe-restored")
    }

    func testOlderSnapshot() throws {
        open("multi-snapshot.zfs")
        let labels = app.outlines.staticTexts
        // Snapshot s2 is the middle row even on v0.9.0, which has no file-history sheet.
        let incremental = labels.matching(NSPredicate(format: "value CONTAINS %@", "changes only"))
        XCTAssertTrue(incremental.firstMatch.waitForExistence(timeout: 10))
        let named = element(labels, containing: "· s2 ·")
        click(named.exists ? named : incremental.firstMatch)
        XCTAssertTrue(element(labels, containing: "only-s2.txt").waitForExistence(timeout: 15))
        click(element(labels, containing: "version.txt"))
        restore(to: evidence.appendingPathComponent("restored"))
        XCTAssertTrue(app.buttons["Show in Finder"].waitForExistence(timeout: 15))
        capture("older-restored")
    }
}
