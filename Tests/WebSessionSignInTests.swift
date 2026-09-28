import XCTest
@testable import Codenotch

/// Opening a sign-in sheet has to leave every site able to become signed in.
///
/// DeepSeek confirms a sign-in itself, by probing the page until it sees an
/// authenticated session, so it is not marked signed in until that happens.
/// Perplexity has no such probe. Waiting for one there meant waiting for
/// something that never runs: the sheet closed and the provider stayed at
/// "needs sign-in" for good, with nothing in the suite to notice.
@MainActor
final class WebSessionSignInTests: XCTestCase {
    /// These run inside the app, so the flags are the installed app's own
    /// preferences — saved and put back rather than left clobbered.
    private let keys = ["perplexity.signedIn", "deepseek.signedIn"]
    private var saved: [String: Any] = [:]

    override func setUp() {
        super.setUp()
        for key in keys {
            saved[key] = UserDefaults.standard.object(forKey: key)
            UserDefaults.standard.removeObject(forKey: key)
        }
    }

    override func tearDown() {
        for key in keys {
            if let value = saved[key] { UserDefaults.standard.set(value, forKey: key) }
            else { UserDefaults.standard.removeObject(forKey: key) }
        }
        super.tearDown()
    }

    func testASiteWithoutAProbeIsSignedInOnceItsSheetOpens() {
        WebSessionProvider(site: Sites.perplexity).signInSheetDidOpen()
        XCTAssertTrue(UserDefaults.standard.bool(forKey: "perplexity.signedIn"),
                      "Perplexity has no probe to confirm a sign-in, so it could never become signed in")
    }

    func testASiteWithAProbeWaitsForItToConfirm() {
        WebSessionProvider(site: Sites.deepSeek).signInSheetDidOpen()
        XCTAssertFalse(UserDefaults.standard.bool(forKey: "deepseek.signedIn"),
                       "opening the sheet is not a sign-in for a site that can confirm one")
    }
}
