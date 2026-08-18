import SwiftUI
import UIKit

/// §9's ceremony, in the words a family reads (`specs/multi-device-v1.md` §13
/// WP6).
///
/// One screen, two ends, and which end this is never asked as a question — it is
/// decided by the door the person came through:
///
/// * From **Settings → Your devices → Add a device**, this phone is the one that
///   is already set up, so it scans and it holds the confirm (§9.2's "the user
///   confirms match on the existing device").
/// * From **the restore flow's "Set up as a new device"**, this phone is the new
///   one, so it shows the code and waits.
///
/// There is no role picker because there is no question: a person holding two
/// phones already knows which is which, and asking them to say so is the kind of
/// step that gets answered wrong.
///
/// The code on screen carries ephemeral link material and rendezvous hints only —
/// §9.1's rule, enforced in core, and the reason this screen may show a QR at all.
/// Nothing here reads, renders or logs an identity secret.
///
/// Presented inside the caller's `NavigationStack` (Settings' or the restore
/// flow's), so it supplies none of its own.
///
/// Mirrors Android's `AddDeviceScreen.kt`.
struct AddDeviceView: View {
    let identity: Identity
    let role: CoreLinkRole
    var expectedPersonId: Data?
    /// Every ending that is not an adoption: a run that was declined, timed out
    /// or threw, and the approving device's own finish. Not a door into the app —
    /// see `LinkCompletion` for why the two must not share one callback.
    var onFinished: () -> Void = {}
    /// Where a phone that was just adopted goes: into the app, never back the way
    /// it came. Defaults to `onFinished`, so a caller with no separate landing
    /// place keeps exactly the behaviour it had. Mirrors Android's `onLinked`,
    /// which defaults to `onBack` for the same reason.
    var onLinked: () -> Void = {}

    @Environment(\.dismiss) private var dismiss
    @StateObject private var session: LinkSession
    @State private var transport: LinkTransport = .lan
    @State private var scannedCode = ""
    @State private var showScan = false
    @State private var showDetails = false
    @State private var readiness: CoreLinkImportReadiness?
    /// The backstop for the same rule "Your devices" gates its button on (§9.5:
    /// only the approving device can sign the roster the new one joins). Asked
    /// here as well because this screen is reachable by other routes, and the
    /// failure it prevents happens at the very END of the ceremony, after two
    /// people have compared six digits.
    @State private var canApprove = true

    private let hasPass = RelayConfigStore.load() != nil

    init(
        identity: Identity,
        role: CoreLinkRole,
        expectedPersonId: Data? = nil,
        onFinished: @escaping () -> Void = {},
        onLinked: (() -> Void)? = nil
    ) {
        self.identity = identity
        self.role = role
        self.expectedPersonId = expectedPersonId
        self.onFinished = onFinished
        self.onLinked = onLinked ?? onFinished
        _session = StateObject(
            wrappedValue: LinkSession(identity: identity, expectedPersonId: expectedPersonId)
        )
    }

    var body: some View {
        Form {
            if session.state.step == .idle {
                beforeYouStart
            } else {
                inProgress
            }
        }
        .navigationTitle("Add a device")
        .accessibilityIdentifier("screen.add-device")
        .sheet(isPresented: $showScan) {
            QRScannerView { code in
                showScan = false
                scannedCode = code
            }
        }
        .task {
            // §9.3: a phone that already holds someone's contacts and messages
            // cannot be adopted. Read once, before anything starts — the answer
            // cannot change during a ceremony, and being told this after comparing
            // six digits with somebody is the wrong end of the run to find out.
            guard role == .newDevice else {
                readiness = .ready
                canApprove = session.canSignRoster()
                return
            }
            readiness = session.importReadiness()
        }
        .onDisappear { session.close() }
    }

    // MARK: - Before the run

    @ViewBuilder
    private var beforeYouStart: some View {
        Section {
            Text(introText)
                .font(.callout)
            if let readiness, readiness != .ready {
                Text(readinessText(readiness))
                    .font(.callout)
                    .foregroundStyle(.red)
            }
            if !canApprove {
                Text("Only the device that approves new devices can add one. Open Your devices there.")
                    .font(.callout)
                    .foregroundStyle(.red)
            }
        }

        if role == .approvingDevice {
            Section("The other device's code") {
                Text("Point the camera at the code on the other phone, or paste it below.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Button {
                    showScan = true
                } label: {
                    Label("Scan the code", systemImage: "qrcode.viewfinder")
                }
                TextField("Code", text: $scannedCode, axis: .vertical)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .accessibilityIdentifier("add-device.code")
            }
        }

        // Advanced, and behind a disclosure, because the answer is right by
        // default: two phones in the same hands are on the same Wi-Fi, and the
        // only reason to choose otherwise is a person linking a phone that is
        // somewhere else.
        Section {
            Button(showDetails ? "Hide details" : "Details") { showDetails.toggle() }
            if showDetails {
                if hasPass {
                    Picker("How should they reach each other?", selection: $transport) {
                        Text("Over Wi-Fi").tag(LinkTransport.lan)
                        Text("Over the internet").tag(LinkTransport.relay)
                    }
                    .pickerStyle(.inline)
                } else {
                    // No pass, so there is no second option to offer — a choice
                    // with one answer is a question that should not be asked.
                    Text("This phone has no pass yet, so it can only link over Wi-Fi.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }

        Section {
            Button("Start") { start() }
                .disabled(!canStart)
                .accessibilityIdentifier("add-device.start")
        }
    }

    private var canStart: Bool {
        switch role {
        case .newDevice:
            return readiness == .ready
        case .approvingDevice:
            return canApprove
                && !scannedCode.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        }
    }

    private func start() {
        // A relay rendezvous needs a pass on both phones; without one the only
        // honest choice is Wi-Fi, so the picker's answer is corrected rather than
        // a ceremony started that cannot post anywhere.
        let chosen = hasPass ? transport : .lan
        switch role {
        case .newDevice:
            session.startAsNewDevice(transport: chosen)
        case .approvingDevice:
            session.startAsApprovingDevice(
                qrText: scannedCode.trimmingCharacters(in: .whitespacesAndNewlines),
                transport: chosen
            )
        }
    }

    // MARK: - During the run

    @ViewBuilder
    private var inProgress: some View {
        let state = session.state

        Section {
            Text(stepText(state.step))
                .font(.headline)
        }

        if role == .newDevice, let qrText = state.qrText, state.step != .done {
            Section {
                if let image = QRCodeGenerator.image(from: qrText, size: 280) {
                    Image(uiImage: image)
                        .interpolation(.none)
                        .resizable()
                        .scaledToFit()
                        .frame(maxWidth: .infinity)
                        .frame(height: 280)
                        .background(RoundedRectangle(cornerRadius: 16).fill(Color.white))
                }
                Text("Hold this up to the other phone.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                // The way through when a camera will not focus, which on an old
                // phone held at arm's length is not rare. The code is ephemeral
                // link material and rendezvous hints (§9.1) — never an identity
                // secret — so a person may copy it and move it across by any means
                // they like.
                Button("Copy the code") { UIPasteboard.general.string = qrText }
            }
        }

        if let sas = state.sas, state.step == .comparingDigits {
            Section {
                Text(verbatim: sas)
                    .font(.system(.largeTitle, design: .monospaced))
                    .frame(maxWidth: .infinity)
                    .accessibilityIdentifier("add-device.sas")
                if state.warnSoftCap {
                    Text("You already have a lot of devices set up.")
                        .font(.caption)
                        .foregroundStyle(.red)
                }
                // §9.2: the buttons exist only on the phone that is already part
                // of this person. The other screen shows the same digits and
                // waits.
                if state.confirmHere {
                    Text("Check that both phones show the same numbers, then answer here.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Button("They match") { session.answerDigits(matches: true) }
                        .accessibilityIdentifier("add-device.match")
                    Button("They are different", role: .destructive) {
                        session.answerDigits(matches: false)
                    }
                }
            }
        }

        // What arrived, counted, with nothing a person cannot act on.
        //
        // Only on the new device. The approving side receives nothing — it sends
        // — so its report carries zeroes by construction, and "Brought over 0
        // contacts and 0 messages" under a successful link reads as a failure to
        // the one person who has to trust that it worked.
        if role == .newDevice, let report = state.report {
            Section {
                Text(broughtOverText(report))
                if report.catchUpChats > 0 {
                    Text("Older messages will keep arriving in the background.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }

        if let ending = outcomeText(state.outcome) {
            Section { Text(ending) }
        } else if state.step == .failed {
            // A ceremony that threw rather than ended has no CoreLinkOutcome to
            // word, and "Stopped" on its own reads as something the person did.
            // The generic line names the two things worth trying.
            Section {
                Text("The setup did not finish. Both phones need to be nearby and awake — try again.")
            }
        }

        Section {
            if state.step == .done || state.step == .failed {
                Button("Done") {
                    session.close()
                    // One button, two meanings: a phone that was just adopted is
                    // set up and belongs in the app, and everything else --
                    // including a run that failed, which this same button ends --
                    // belongs back where it came from.
                    if LinkCompletion.entersApp(role: role, step: state.step) {
                        onLinked()
                    } else {
                        onFinished()
                    }
                    dismiss()
                }
            } else {
                Button("Stop", role: .destructive) { session.cancel() }
            }
        }
    }

    // MARK: - Copy

    private var introText: String {
        switch role {
        case .newDevice:
            return String(
                localized: "Show this code to the device you already use. Then check that both screens show the same numbers."
            )
        case .approvingDevice:
            return String(
                localized: "Get the other device ready first. When it shows a code, point this camera at it. Then check that both screens show the same numbers."
            )
        }
    }

    private func broughtOverText(_ report: LinkReport) -> String {
        String(
            localized: "Brought over \(Int(report.contacts)) contacts and \(Int(report.messages)) messages."
        )
    }
}

/// Why this phone cannot be adopted. Never reached for a ready store.
func readinessText(_ readiness: CoreLinkImportReadiness) -> String {
    switch readiness {
    case .ready, .storeHoldsSomeone:
        return String(
            localized: "This phone already has contacts and messages on it, so it cannot be added as a new phone. To use this phone anyway, remove the app, install it again, and choose \"Set up as a new device\"."
        )
    case .storeHoldsAnotherPerson:
        return String(localized: "This phone is already set up for someone else.")
    }
}

func stepText(_ step: LinkStep) -> String {
    switch step {
    case .idle, .waitingForPeer:
        return String(localized: "Waiting for the other phone…")
    case .handshaking:
        return String(localized: "Setting up a private channel…")
    case .comparingDigits:
        return String(localized: "Compare the numbers")
    case .carryingBootstrap:
        return String(localized: "Copying contacts and recent messages…")
    case .activating:
        return String(localized: "Finishing up…")
    case .done:
        return String(localized: "Linked")
    case .failed:
        return String(localized: "Stopped")
    }
}

/// Every ending the core names, said once. `channelReady` has no line of its own
/// because the run kept going past it — the counts above are what happened next.
func outcomeText(_ outcome: CoreLinkOutcome?) -> String? {
    switch outcome {
    case nil, .channelReady:
        return nil
    case .declined:
        return String(localized: "The numbers did not match.")
    case .cancelled:
        return String(localized: "One of the phones stopped.")
    case .timedOut:
        return String(localized: "The other phone never answered.")
    case .qrExpired:
        return String(localized: "The code had already expired.")
    case .deviceCapReached:
        return String(localized: "You already have as many devices as you can have.")
    case .handshakeFailed:
        return String(localized: "The private channel could not be set up.")
    case .protocolError:
        return String(localized: "The other phone sent something unexpected.")
    }
}
