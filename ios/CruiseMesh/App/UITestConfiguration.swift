import Foundation

/// Explicit, process-wide configuration for XCUITest launches.
///
/// The UI-test runner is a separate process, so launch arguments/environment
/// are the only reliable way to select deterministic state before SwiftUI and
/// the persistence singletons initialize. Normal launches never enter this
/// path. Each test supplies a unique run id, which keeps its preferences,
/// Keychain identity, and SQLite database separate from both other tests and a
/// developer's ordinary Simulator data.
enum UITestConfiguration {
    enum Scenario: String {
        case terms
        case onboarding
        case homeEmpty = "home-empty"
        case chat
        case chatListActions = "chat-list-actions"
        case chatLateArrival = "chat-late-arrival"

        var termsAccepted: Bool { self != .terms }
        var onboardingCompleted: Bool {
            switch self {
            case .homeEmpty, .chat, .chatListActions, .chatLateArrival:
                return true
            case .terms, .onboarding:
                return false
            }
        }
    }

    static let isEnabled: Bool = {
#if DEBUG
        return ProcessInfo.processInfo.arguments.contains("--ui-testing")
#else
        // A distribution build must never expose fixture state or suppress
        // production services, even if somebody manages to supply arguments.
        return false
#endif
    }()

    static let scenario: Scenario? = {
        guard isEnabled else { return nil }
        let arguments = ProcessInfo.processInfo.arguments
        guard let flag = arguments.firstIndex(of: "--ui-scenario"),
              arguments.indices.contains(flag + 1)
        else { return .terms }
        return Scenario(rawValue: arguments[flag + 1]) ?? .terms
    }()

    static let runIdentifier: String = {
        let raw = ProcessInfo.processInfo.environment["CRUISEMESH_UI_TEST_RUN_ID"]
            ?? "process-\(ProcessInfo.processInfo.processIdentifier)"
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-_"))
        let sanitized = raw.unicodeScalars.map { allowed.contains($0) ? Character(String($0)) : "-" }
        return String(sanitized.prefix(80))
    }()

    static let defaults: UserDefaults = {
        guard isEnabled else { return .standard }
        let suite = "com.cruisemesh.app.uitests.\(runIdentifier)"
        guard let defaults = UserDefaults(suiteName: suite) else {
            preconditionFailure("Could not create isolated UI-test preferences suite")
        }
        return defaults
    }()

    static var databaseURL: URL? {
        guard isEnabled else { return nil }
        return testFilesRoot
            .appendingPathComponent("messages.sqlite")
    }

    static var avatarURL: URL? {
        guard isEnabled else { return nil }
        return testFilesRoot
            .appendingPathComponent("profile", isDirectory: true)
            .appendingPathComponent("avatar.jpg")
    }

    private static var testFilesRoot: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            .appendingPathComponent("CruiseMeshUITests", isDirectory: true)
            .appendingPathComponent(runIdentifier, isDirectory: true)
    }

    static var identityAccountSuffix: String? {
        isEnabled ? ".uitests.\(runIdentifier)" : nil
    }

    /// Seeds only deterministic, isolated state. Live radios/network are never
    /// needed to construct these scenarios.
    static func prepareFixtures(identity: Identity) {
        guard let scenario else { return }
        switch scenario {
        case .terms, .onboarding:
            break
        case .homeEmpty:
            ProfileStore.saveDisplayName("UI Tester")
        case .chat:
            ProfileStore.saveDisplayName("UI Tester")
            _ = seedContact(name: "Bob")
        case .chatListActions:
            ProfileStore.saveDisplayName("UI Tester")
            let contact = seedContact(name: "Robert", nickname: "Dad")
            insertMessage(
                chatId: contact.userId,
                senderUserId: contact.userId,
                lamport: 1,
                text: "Unread from Dad"
            )
        case .chatLateArrival:
            ProfileStore.saveDisplayName("UI Tester")
            let contact = seedContact(name: "Bob")
            for index in 1...32 {
                insertMessage(
                    chatId: contact.userId,
                    senderUserId: index.isMultiple(of: 3) ? identity.userId : contact.userId,
                    lamport: UInt64(index),
                    text: "History message \(index)"
                )
            }
        }
    }

    static func injectIncomingMessage(contact: Contact, text: String) {
        guard scenario == .chatLateArrival else { return }
        insertMessage(
            chatId: contact.userId,
            senderUserId: contact.userId,
            lamport: 1_000,
            text: text
        )
        ChatEvents.notifyChatChanged(contact.userId)
    }

    @discardableResult
    private static func seedContact(name: String, nickname: String? = nil) -> Contact {
        let peer = generateIdentity()
        let contact = Contact(
            userId: peer.userId,
            name: name,
            signPk: peer.signPk,
            agreePk: peer.agreePk,
            relayUrl: nil,
            relayToken: nil,
            nickname: nil
        )
        let store = AppStore.get()
        do {
            try store.upsertContact(contact: contact)
            if let nickname {
                let nicknameSaved = try store.setContactNickname(
                    userId: contact.userId,
                    nickname: nickname
                )
                guard nicknameSaved else {
                    preconditionFailure("Could not seed UI-test contact nickname")
                }
            }
        } catch {
            preconditionFailure("Could not seed UI-test contact: \(error)")
        }
        guard let stored = try? store.getContact(userId: contact.userId) else {
            preconditionFailure("Could not reload seeded UI-test contact")
        }
        return stored
    }

    private static func insertMessage(
        chatId: Data,
        senderUserId: Data,
        lamport: UInt64,
        text: String
    ) {
        do {
            _ = try AppStore.get().insertMessage(message: StoredMessage(
                chatId: chatId,
                senderUserId: senderUserId,
                lamport: lamport,
                timestamp: 1_700_000_000_000 + Int64(lamport) * 1_000,
                kind: ProtocolKind.text,
                payload: Data(text.utf8)
            ))
        } catch {
            preconditionFailure("Could not seed UI-test message: \(error)")
        }
    }
}

/// Application preferences use this indirection so XCUITest launches cannot
/// mutate the app's ordinary defaults domain. It is intentionally tiny: all
/// production behavior still resolves to `UserDefaults.standard`.
enum AppDefaults {
    static var current: UserDefaults { UITestConfiguration.defaults }
}
