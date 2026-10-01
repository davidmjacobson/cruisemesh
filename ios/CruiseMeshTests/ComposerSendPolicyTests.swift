import XCTest
@testable import CruiseMesh

/// The composer's data-loss invariant: a send that did not reach the durable
/// local transaction leaves every character the user typed exactly where it was.
///
/// Field report this pins: type a message, tap send, the send fails, the text
/// vanishes with nothing stored, nothing queued and nothing said. Both chat
/// views used to clear the field on the way past the send call, so the only
/// thing standing between a failed send and lost words was that the call
/// happened not to fail. `ComposerSendPolicy` is now the sole place that can
/// empty the composer, and these tests are what hold it to that.
final class ComposerSendPolicyTests: XCTestCase {
    private let photo = Data([1, 2, 3])

    func testFailedTextSendKeepsTheDraftAndReportsTheFailure() {
        var sentText: String?

        let outcome = ComposerSendPolicy.attempt(
            draft: "meet you on the pool deck",
            pendingPhoto: nil,
            sendPhoto: { _, _ in
                XCTFail("no photo staged")
                return .failed
            },
            sendText: { text in
                sentText = text
                return .failed
            }
        )

        XCTAssertEqual(sentText, "meet you on the pool deck")
        XCTAssertEqual(outcome.draft, "meet you on the pool deck")
        XCTAssertNil(outcome.pendingPhoto)
        XCTAssertEqual(outcome.status, .notQueued)
    }

    func testStoredTextSendClearsTheDraft() {
        let outcome = ComposerSendPolicy.attempt(
            draft: "meet you on the pool deck",
            pendingPhoto: nil,
            sendPhoto: { _, _ in
                XCTFail("no photo staged")
                return .failed
            },
            sendText: { _ in .stored }
        )

        XCTAssertEqual(outcome.draft, "")
        XCTAssertNil(outcome.pendingPhoto)
        XCTAssertEqual(outcome.status, .queued)
    }

    func testFailedSendHandsBackTheUntrimmedDraftSoRetryingCostsOneTap() {
        // What goes on the wire is trimmed; what stays in the field is not, so
        // any leading or trailing whitespace survives a failure untouched.
        let typed = "  still here\n"

        let outcome = ComposerSendPolicy.attempt(
            draft: typed,
            pendingPhoto: nil,
            sendPhoto: { _, _ in
                XCTFail("no photo staged")
                return .failed
            },
            sendText: { text in
                XCTAssertEqual(text, "still here")
                return .failed
            }
        )

        XCTAssertEqual(outcome.draft, typed)
    }

    func testFailedPhotoSendKeepsBothTheStagedPhotoAndItsCaption() {
        var sentCaption: String?

        let outcome = ComposerSendPolicy.attempt(
            draft: "  from the top deck  ",
            pendingPhoto: photo,
            sendPhoto: { staged, caption in
                XCTAssertEqual(staged, self.photo)
                sentCaption = caption
                return .failed
            },
            sendText: { _ in
                XCTFail("a staged photo must win over bare text")
                return .failed
            }
        )

        XCTAssertEqual(sentCaption, "from the top deck")
        XCTAssertEqual(outcome.draft, "  from the top deck  ")
        XCTAssertEqual(outcome.pendingPhoto, photo)
        XCTAssertEqual(outcome.status, .notQueued)
    }

    func testStoredPhotoSendClearsBothTheStagedPhotoAndTheCaption() {
        let outcome = ComposerSendPolicy.attempt(
            draft: "from the top deck",
            pendingPhoto: photo,
            sendPhoto: { _, _ in .stored },
            sendText: { _ in
                XCTFail("a staged photo must win over bare text")
                return .failed
            }
        )

        XCTAssertEqual(outcome.draft, "")
        XCTAssertNil(outcome.pendingPhoto)
        XCTAssertEqual(outcome.status, .queued)
    }

    func testEmptyComposerAttemptsNothingAndIsNotReportedAsAFailure() {
        var attempts = 0

        let outcome = ComposerSendPolicy.attempt(
            draft: "   \n ",
            pendingPhoto: nil,
            sendPhoto: { _, _ in
                attempts += 1
                return .stored
            },
            sendText: { _ in
                attempts += 1
                return .stored
            }
        )

        XCTAssertEqual(attempts, 0)
        XCTAssertEqual(outcome.draft, "   \n ")
        XCTAssertEqual(outcome.status, .nothingToSend)
    }

    func testDraftThatFailedOnceSurvivesEveryRetryUntilOneIsStored() {
        var draft = "keep me"
        var attempt = 0

        for _ in 0..<3 {
            let outcome = ComposerSendPolicy.attempt(
                draft: draft,
                pendingPhoto: nil,
                sendPhoto: { _, _ in
                    XCTFail("no photo staged")
                    return .failed
                },
                sendText: { _ in
                    attempt += 1
                    return attempt < 3 ? .failed : .stored
                }
            )
            draft = outcome.draft
        }

        XCTAssertEqual(attempt, 3)
        XCTAssertTrue(draft.isEmpty, "the composer must not empty until a send is stored")
    }

    /// The end the policy depends on: the real sender really does report
    /// `.failed` when the core stores nothing, rather than swallowing the
    /// error into a `Void` return the way it used to.
    func testRealSenderReportsFailedWhenTheEnvelopeCannotBeSealed() throws {
        let store = try MessageStore.open(path: ":memory:")
        let alice = generateIdentity()
        let bob = generateIdentity()
        let unsealable = Contact(
            userId: bob.userId,
            name: "Invalid key",
            signPk: bob.signPk,
            agreePk: Data([1]),
            relayUrl: nil,
            relayToken: nil
        )

        var draft = "keep this draft"
        let outcome = ComposerSendPolicy.attempt(
            draft: draft,
            pendingPhoto: nil,
            sendPhoto: { _, _ in
                XCTFail("no photo staged")
                return .failed
            },
            sendText: { text in
                RealMeshSender(store: store, identity: alice)
                    .sendText(contact: unsealable, text: text, replyToMsgId: nil)
            }
        )
        draft = outcome.draft

        XCTAssertEqual(outcome.status, .notQueued)
        XCTAssertEqual(draft, "keep this draft")
        XCTAssertEqual(try store.messagesForChat(chatId: unsealable.userId).count, 0)
        XCTAssertEqual(
            try store.outboundEnvelopesAfter(
                chatId: unsealable.userId,
                senderUserId: alice.userId,
                afterLamport: 0
            ).count,
            0
        )
    }
}
