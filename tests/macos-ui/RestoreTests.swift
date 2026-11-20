import AppKit
import CryptoKit
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
        click(app.toolbars.buttons["Restore…"])
        XCTAssertTrue(app.buttons["CancelButton"].waitForExistence(timeout: 10))
        goTo(directory)
        click(app.buttons["OKButton"])
    }

    private func capture(_ name: String) throws {
        try app.screenshot().pngRepresentation.write(to: evidence.appendingPathComponent(name + ".png"))
    }

    private func check(_ file: URL, expected: String) throws {
        let bytes = try Data(contentsOf: file)
        let actual = SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
        XCTAssertEqual(actual, expected, file.path)
        let record = ["file": file.lastPathComponent, "expected": expected, "actual": actual]
        let json = try JSONSerialization.data(withJSONObject: record, options: [.prettyPrinted, .sortedKeys])
        try json.write(to: file.appendingPathExtension("verification.json"))
    }

    func testEncryptedRestoreAndOccupiedDestination() throws {
        open("encrypted-raw-s1.zfs")
        let field = app.secureTextFields.firstMatch
        click(field)
        field.typeText("incorrect-public-test-key\n")
        XCTAssertTrue(element(app.staticTexts, containing: "unlock this backup").waitForExistence(timeout: 10))
        try capture("wrong-key")
        click(app.buttons["OK"])
        click(field)
        field.typeText("zfs-send-fixture-passphrase\n")
        click(element(app.descendants(matching: .tableRow), containing: "docs"))
        click(app.buttons["Open Folder"])
        click(element(app.descendants(matching: .tableRow), containing: "hello.txt"))
        let recovered = evidence.appendingPathComponent("restored")
        restore(to: recovered)
        XCTAssertTrue(app.buttons["Show in Finder"].waitForExistence(timeout: 15))
        try check(recovered.appendingPathComponent("hello.txt"), expected: "6e219bc096755477b1a30301446ac364f6291f16ce381d561947cdecd3fab265")
        try capture("encrypted-restored")

        let occupied = evidence.appendingPathComponent("occupied")
        let sentinel = occupied.appendingPathComponent("hello.txt")
        try Data("keep this existing file\n".utf8).write(to: sentinel, options: .withoutOverwriting)
        restore(to: occupied)
        click(app.buttons["Replace"])
        XCTAssertTrue(element(app.staticTexts, containing: "already there").waitForExistence(timeout: 10))
        try check(sentinel, expected: "880b6e73a4f158b5df9ccc251d518fa5f385008ac5f26153e9b8451077fdcbc0")
        try capture("same-name-refused")
    }

    func testOlderSnapshot() throws {
        open("multi-snapshot.zfs")
        let rows = app.descendants(matching: .tableRow)
        // Snapshot s2 is the middle row even on v0.9.0, which has no file-history sheet.
        let incremental = rows.matching(NSPredicate(format: "value CONTAINS %@", "changes only"))
        XCTAssertTrue(incremental.firstMatch.waitForExistence(timeout: 10))
        let named = element(rows, containing: "· s2 ·")
        click(named.exists ? named : incremental.firstMatch)
        XCTAssertTrue(element(rows, containing: "only-s2.txt").waitForExistence(timeout: 15))
        click(element(rows, containing: "version.txt"))
        restore(to: evidence.appendingPathComponent("restored"))
        XCTAssertTrue(app.buttons["Show in Finder"].waitForExistence(timeout: 15))
        try check(evidence.appendingPathComponent("restored/version.txt"), expected: "4a5b121833313a53cc3120afd66cb662c2e7b23f839ef547e9e58576c52e48e0")
        try capture("older-restored")
    }
}
