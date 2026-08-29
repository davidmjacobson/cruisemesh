package com.cruisemesh.app.devicelink

import android.content.Context
import android.util.Log
import com.cruisemesh.app.AppStore
import com.cruisemesh.app.identity.DeviceKeyStore
import com.cruisemesh.app.relay.RelayConfigStore
import java.io.IOException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import uniffi.cruisemesh_core.CoreLinkApprovingDevice
import uniffi.cruisemesh_core.CoreLinkImportReadiness
import uniffi.cruisemesh_core.CoreLinkLane
import uniffi.cruisemesh_core.CoreLinkNewDevice
import uniffi.cruisemesh_core.CoreLinkOutcome
import uniffi.cruisemesh_core.CoreLinkRole
import uniffi.cruisemesh_core.CoreLinkSummary
import uniffi.cruisemesh_core.DeviceKeypair
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.Roster
import uniffi.cruisemesh_core.coreLinkActivationAck
import uniffi.cruisemesh_core.coreLinkBootstrapChunks
import uniffi.cruisemesh_core.coreLinkBootstrapDecode
import uniffi.cruisemesh_core.coreLinkBootstrapEncode
import uniffi.cruisemesh_core.coreLinkBootstrapJoin
import uniffi.cruisemesh_core.coreLinkDeviceOffer
import uniffi.cruisemesh_core.coreLinkGenesisRoster
import uniffi.cruisemesh_core.coreLinkOpenActivationAck
import uniffi.cruisemesh_core.coreLinkOpenDeviceOffer
import uniffi.cruisemesh_core.coreLinkSignNewDeviceRoster
import uniffi.cruisemesh_core.coreRosterDeviceIds

/** How the two phones reach each other (`specs/multi-device-v1.md` §9.2). */
enum class LinkTransport { LAN, RELAY }

/** How far one run has got. Rendered from string resources, never from here. */
enum class LinkStep {
    IDLE,
    WAITING_FOR_PEER,
    HANDSHAKING,
    COMPARING_DIGITS,
    CARRYING_BOOTSTRAP,
    ACTIVATING,
    DONE,
    FAILED,
}

/** What landed, for the sentence the person reads when the run finishes. */
data class LinkReport(
    val deviceIdHex: String,
    val rosterHeadHex: String,
    val rosterSeq: ULong,
    val contacts: UInt,
    val groups: UInt,
    val messages: UInt,
    val catchUpChats: Int,
)

data class LinkState(
    val role: CoreLinkRole? = null,
    val transport: LinkTransport? = null,
    val step: LinkStep = LinkStep.IDLE,
    val qrText: String? = null,
    val sas: String? = null,
    val confirmHere: Boolean = false,
    val warnSoftCap: Boolean = false,
    val outcome: CoreLinkOutcome? = null,
    val report: LinkReport? = null,
    /** Raw diagnostic text. Developer-facing, never family-facing copy. */
    val failure: String? = null,
)

/**
 * One end-to-end run of §9: the ceremony a family actually performs.
 *
 * Built for §13's WP3 gate and promoted, unchanged in substance, to be the
 * driver behind WP6's "Add a device" — the same core objects, the same
 * ordering, the same refusals. Nothing about the ceremony was ever
 * developer-only; only the words were, and those now live in `strings.xml`
 * beside every other thing a family reads.
 *
 * # What it deliberately does not do
 *
 * It never touches [com.cruisemesh.app.identity.IdentityStore]. A linked device
 * receives the person's *inbox* key in the bootstrap and never the person root
 * signing secret, which §14.2 keeps inside the encrypted backup -- so there is
 * no complete [Identity] for a new device to adopt, and inventing one here
 * would be inventing the thing the spec spent §3 taking away. Authoring as the
 * person from a second device is WP4's per-device author streams; until then a
 * newly linked phone reads what it was given and stays quiet.
 *
 * @param expectedPersonId the person the new device believes it is joining,
 *   read out of the `.cmbak` the restore flow opened (§9's closing paragraph).
 *   Core refuses a bootstrap for anybody else. `null` means "whoever adopts
 *   this factory-fresh phone", which is all the ceremony can promise when no
 *   backup was opened first.
 */
class LinkSession(
    context: Context,
    private val identity: Identity,
    private val expectedPersonId: ByteArray? = null,
) {
    private val appContext = context.applicationContext
    private val store: MessageStore = AppStore.get(appContext)

    private val _state = MutableStateFlow(LinkState())
    val state: StateFlow<LinkState> = _state.asStateFlow()

    @Volatile
    private var decision = LinkConfirmDecision.WAITING

    @Volatile
    private var cancelRequested = false

    @Volatile
    private var worker: Thread? = null

    /**
     * Whether this phone may be adopted as a new device at all (§9.3).
     *
     * Asked before a ceremony starts rather than after one, because the answer
     * cannot change during it and the alternative is a person holding two
     * phones together, comparing six digits, and only then being told this one
     * was never eligible.
     */
    fun importReadiness(): CoreLinkImportReadiness =
        runCatching { store.linkImportReadiness(expectedPersonId) }
            .getOrElse {
                Log.w(TAG, "could not read link import readiness", it)
                CoreLinkImportReadiness.STORE_HOLDS_SOMEONE
            }

    /**
     * Whether this install can finish §9.5 at all: it holds the roster-signing
     * role, or there is no roster yet and it is about to mint §3's genesis and
     * become device one.
     *
     * The backstop behind "Your devices"' gate on the same rule. Both are here
     * rather than only there because the signature is the LAST step of the
     * ceremony: without this, a sibling could be walked through a code, a
     * camera and six digits and fail after the person had done everything
     * right. Fails closed -- a store that cannot answer is not one to start a
     * ceremony on the strength of.
     */
    fun canSignRoster(): Boolean = runCatching {
        val roster = store.ownRoster() ?: return@runCatching true
        val ownDeviceId = DeviceKeyStore.load(appContext)?.deviceId ?: return@runCatching false
        roster.approvingDeviceId.contentEquals(ownDeviceId)
    }.getOrElse {
        Log.w(TAG, "could not read this device's roster role", it)
        false
    }

    /** §9.1: mint an offer, show it, and wait to be adopted. */
    fun startAsNewDevice(transport: LinkTransport) {
        launch(CoreLinkRole.NEW_DEVICE, transport) { runNewDevice(transport) }
    }

    /** §9.2: scan an offer and, if the digits match, adopt what showed it. */
    fun startAsApprovingDevice(qrText: String, transport: LinkTransport) {
        launch(CoreLinkRole.APPROVING_DEVICE, transport) { runApprovingDevice(qrText, transport) }
    }

    /** §9.2's explicit action. Only ever reachable on the approving device. */
    fun answerDigits(matches: Boolean) {
        decision = if (matches) LinkConfirmDecision.MATCHES else LinkConfirmDecision.DOES_NOT_MATCH
    }

    fun cancel() {
        cancelRequested = true
    }

    fun close() {
        cancel()
        worker?.interrupt()
    }

    /**
     * Back to the start after a run has ended, so "start over" is one tap
     * rather than leaving the screen and coming back in by the same door.
     *
     * Only ever reached from an ended run: the screen offers it beside "Done",
     * on the step a worker writes as its last act before returning. That is
     * what makes dropping the [worker] reference safe here and nowhere else --
     * [launch]'s "one run at a time" guard reads it, so clearing it mid-run
     * would let a second ceremony start underneath the first.
     */
    fun reset() {
        close()
        worker = null
        decision = LinkConfirmDecision.WAITING
        cancelRequested = false
        _state.value = LinkState()
    }

    private fun launch(role: CoreLinkRole, transport: LinkTransport, body: () -> Unit) {
        if (worker?.isAlive == true) return
        decision = LinkConfirmDecision.WAITING
        cancelRequested = false
        _state.value = LinkState(
            role = role,
            transport = transport,
            step = LinkStep.WAITING_FOR_PEER,
        )
        worker = Thread {
            try {
                body()
            } catch (e: Exception) {
                Log.w(TAG, "link ceremony failed", e)
                _state.value = _state.value.copy(
                    step = LinkStep.FAILED,
                    failure = e.toString(),
                )
            }
        }.apply { isDaemon = true; start() }
    }

    // -----------------------------------------------------------------------
    // §9.1 + §9.3-§9.4(b): the device being adopted
    // -----------------------------------------------------------------------

    private fun runNewDevice(transport: LinkTransport) {
        val listener = if (transport == LinkTransport.LAN) LinkLanListener.open() else null
        val relayConfig = if (transport == LinkTransport.RELAY) requireRelay() else null
        val newDevice = CoreLinkNewDevice(
            // §9.1 and DL-5: this device's own endpoints, and nothing it has
            // merely observed about anyone else's.
            listener?.endpoints.orEmpty(),
            relayConfig?.let { listOf(it.relayUrl) }.orEmpty(),
            now(),
            null,
        )
        newDevice.use {
            val wire = when (transport) {
                LinkTransport.LAN -> LinkLanListener.accepting(listener!!)
                LinkTransport.RELAY -> LinkRelayWire(
                    config = relayConfig!!,
                    rendezvousId = newDevice.rendezvousId(),
                    sendLane = CoreLinkLane.TO_APPROVING_DEVICE,
                    receiveLane = CoreLinkLane.TO_NEW_DEVICE,
                    clock = ::now,
                    sleep = ::pause,
                )
            }
            wire.use {
                val machine = NewDeviceMachine(newDevice)
                _state.value = _state.value.copy(qrText = newDevice.qrText())
                val summary = drive(machine, wire)
                if (summary.outcome != CoreLinkOutcome.CHANNEL_READY) return
                adoptFromChannel(machine, wire, summary)
            }
        }
    }

    /**
     * §9.3 and §9.4 on the new device: go silent, name our keys, take the
     * export, acknowledge the exact roster head, and only then become visible.
     */
    private fun adoptFromChannel(machine: LinkMachine, wire: LinkWire, summary: CoreLinkSummary) {
        val binding = summary.channelBinding ?: error("a ready channel always has a binding")
        val device = DeviceKeyStore.loadOrCreate(appContext)

        // §9.4: the silence starts here, before the export is on the wire --
        // not once it has landed. Everything from this line until the
        // acknowledgement is a device the mesh cannot hear. The binding goes in
        // with it, so the export that arrives has to be THIS ceremony's.
        store.beginLinkActivation(binding, now())
        // From this line the window is open, and every path out of it that is
        // not `completeLinkActivation` has to close it again -- or a ceremony
        // that failed leaves this phone permanently silent with no way back
        // except reinstalling the app.
        try {
            // And the radios go down with it. Core refuses everything it holds
            // from this moment; [LinkVisibility] is how the part core cannot
            // see -- this phone's own BLE advertiser and LAN registration --
            // hears about it. The change hops the mesh service's handler, so
            // this waits for it to have actually happened rather than assuming
            // it: a frame sent in that gap goes out from a phone that is still
            // advertising, which is the one thing §9.4 forbids.
            LinkVisibility.refresh(store)
            if (!LinkVisibility.awaitApplied(false, VISIBILITY_APPLY_TIMEOUT_MS)) {
                throw IOException("this phone did not go quiet in time")
            }

            _state.value = _state.value.copy(step = LinkStep.CARRYING_BOOTSTRAP)
            wire.send(machine.seal(coreLinkDeviceOffer(device.signSk, device.agreePk, binding)))

            val bootstrap =
                coreLinkBootstrapDecode(coreLinkBootstrapJoin(collectBootstrap(machine, wire)))
            // The person id the restore flow read out of the `.cmbak`, when
            // there was one: core refuses a bootstrap from anybody else, so a
            // person who scanned the wrong phone's code is told before their
            // store is written to rather than afterwards. `null` falls back to
            // core's factory-fresh rule alone, which is all a phone with no
            // backup open can check.
            val import =
                store.importLinkBootstrap(bootstrap, device.signPk, expectedPersonId, now())

            _state.value = _state.value.copy(step = LinkStep.ACTIVATING)
            // §9.4(b): the acknowledgement goes out first and the device becomes
            // visible second. The other ordering would make a device visible on
            // the strength of a message that never left.
            wire.send(machine.seal(coreLinkActivationAck(device.signSk, import.rosterHead, binding)))
            store.completeLinkActivation(import.rosterHead, now())
            store.ownRoster()?.let { rememberRosterSeen(it) }
            // Only now, on the far side of the acknowledgement: this phone is a
            // device of this person, so it takes their name and photo and stops
            // being a phone that has never been set up. A ceremony that failed
            // reaches the abandon path below instead and leaves both untouched.
            LinkAdoption.adopted(appContext, import.profile)
            // Visible from here, radios included.
            LinkVisibility.refresh(store)

            // The two-phone smoke script's gate for §13 WP6 reads this line and
            // its twin on the approving device, and compares the heads. A head
            // is a hash of a public document and a device id is derived from a
            // public key, so neither is a secret -- but no identity secret, QR
            // payload or SAS ever appears here, and none may be added.
            Log.i(
                TAG,
                "link complete: role=newDevice deviceId=${hex(import.ownDeviceId)} " +
                    "rosterHead=${hex(import.rosterHead)} contacts=${import.contactsImported} " +
                    "messages=${import.messagesImported}",
            )

            _state.value = _state.value.copy(
                step = LinkStep.DONE,
                report = LinkReport(
                    deviceIdHex = hex(import.ownDeviceId),
                    rosterHeadHex = hex(import.rosterHead),
                    rosterSeq = bootstrap.roster.seq,
                    contacts = import.contactsImported,
                    groups = import.groupsImported,
                    messages = import.messagesImported,
                    catchUpChats = import.catchUp.size,
                ),
            )
        } catch (e: Throwable) {
            abandonActivation()
            throw e
        }
    }

    /**
     * Give §9.4's gates back after a ceremony that did not finish.
     *
     * Every failure inside the window comes through here: a declined confirm, a
     * dropped socket, a bootstrap that never finished arriving, a person who
     * tapped Stop, an import core refused. The core call is a no-op on a store
     * that never opened a window and refuses outright on one that completed, so
     * it is safe to call without first asking where the ceremony got to.
     */
    private fun abandonActivation() {
        runCatching { store.abandonLinkActivation(now()) }
            .onFailure { Log.w(TAG, "could not reopen the gates after a failed link", it) }
        LinkVisibility.refresh(store)
    }

    /**
     * Take sealed chunks until they make a whole export.
     *
     * The completeness test is the core's own reassembly, not a header this
     * shell learned to read: [coreLinkBootstrapJoin] refuses a stream that is
     * short, reordered, or duplicated, so "it joined" is exactly "it is all
     * here" and the shell holds no opinion about the format.
     */
    private fun collectBootstrap(machine: LinkMachine, wire: LinkWire): List<ByteArray> {
        val chunks = mutableListOf<ByteArray>()
        val deadline = now() + BOOTSTRAP_DEADLINE_MS
        while (now() < deadline) {
            if (cancelRequested) throw IOException("cancelled while taking the bootstrap")
            val frame = wire.receive(RECEIVE_WAIT_MS) ?: continue
            chunks += machine.open(frame)
            if (runCatching { coreLinkBootstrapJoin(chunks) }.isSuccess) return chunks
        }
        throw IOException("the bootstrap never finished arriving")
    }

    // -----------------------------------------------------------------------
    // §9.2 + §9.3-§9.4(a): the device doing the adopting
    // -----------------------------------------------------------------------

    private fun runApprovingDevice(qrText: String, transport: LinkTransport) {
        val device = DeviceKeyStore.loadOrCreate(appContext)
        val roster = ownRoster(device)
        val approver = CoreLinkApprovingDevice.scan(qrText, roster.devices.size.toUInt(), null)
        approver.use {
            val rendezvous = approver.rendezvous()
            val wire = when (transport) {
                LinkTransport.LAN -> LinkLanListener.connect(
                    rendezvous.lanEndpoints,
                    CONNECT_TIMEOUT_MS,
                )
                LinkTransport.RELAY -> LinkRelayWire(
                    config = requireRelay(),
                    rendezvousId = approver.rendezvousId(),
                    sendLane = CoreLinkLane.TO_NEW_DEVICE,
                    receiveLane = CoreLinkLane.TO_APPROVING_DEVICE,
                    clock = ::now,
                    sleep = ::pause,
                )
            }
            wire.use {
                val machine = ApprovingDeviceMachine(approver)
                val summary = drive(machine, wire)
                if (summary.outcome != CoreLinkOutcome.CHANNEL_READY) return
                adoptOverChannel(machine, wire, summary, device, roster)
            }
        }
    }

    /** §9.3 and §9.4(a): sign the roster at `seq + 1` and stream the export. */
    private fun adoptOverChannel(
        machine: LinkMachine,
        wire: LinkWire,
        summary: CoreLinkSummary,
        device: DeviceKeypair,
        roster: Roster,
    ) {
        val binding = summary.channelBinding ?: error("a ready channel always has a binding")

        _state.value = _state.value.copy(step = LinkStep.CARRYING_BOOTSTRAP)
        val offer = coreLinkOpenDeviceOffer(machine.open(awaitFrame(wire)), binding)
        val update = coreLinkSignNewDeviceRoster(
            roster,
            identity.signPk,
            device.signSk,
            offer.deviceSignPk,
            offer.deviceAgreePk,
        )

        // Everything older than the head is WP4's catch-up, not this
        // ceremony's. Signed with the roster-signing device's key and bound to this
        // channel, so the phone on the other end can tell this export from a
        // file. 0 = the core's own defaults for head size and lifetime.
        val bootstrap = store.buildLinkBootstrap(
            identity,
            // The person's own name and photo, so the phone being adopted never
            // has to ask them who they are (§9.3). Read here, on the device that
            // has the answer; written on the other side by [LinkAdoption].
            LinkAdoption.profileOf(appContext),
            update.roster,
            device.signSk,
            binding,
            0uL,
            0L,
            now(),
        )
        val payload = coreLinkBootstrapEncode(bootstrap)
        for (chunk in coreLinkBootstrapChunks(payload)) {
            wire.send(machine.seal(chunk))
        }

        _state.value = _state.value.copy(step = LinkStep.ACTIVATING)
        // The acknowledgement must come from the device that made the offer,
        // not merely from some device the roster lists: on a fleet that already
        // has siblings, any of them would otherwise satisfy the check and this
        // side would record a link the new phone never finished.
        val ack = coreLinkOpenActivationAck(
            machine.open(awaitFrame(wire)),
            update.roster,
            offer.deviceSignPk,
            binding,
        )
        // The new device has acknowledged the exact head, so this fleet is now
        // two devices and both of them know it.
        store.adoptOwnRoster(update.roster, identity.signPk, device.deviceId)
        rememberRosterSeen(update.roster)

        // §9.5: "roster gossips to contacts". Fired here, on the approving
        // device, because this is the line where a fleet larger than one device
        // becomes real -- and fired ONLY here, not on the new device as well.
        // The newly adopted phone imported the contact list but not the ledger
        // of who has already been told, so an announcement from it would seal a
        // second identical copy of the same document to every contact. One
        // telling is the whole product of this call.
        //
        // Core makes that a rule rather than an arrangement between call sites:
        // `announce_own_roster` returns the empty shape unless the identity it
        // is given is the person the roster is about, and a linked sibling signs
        // its mail with a per-device identity. So a sibling's routine passes --
        // relay, mesh start -- are already silent, and this remains the only
        // routine announcer. A sibling that ever does hold the person identity
        // is a fallback, not a second voice: the ledger makes its copy a no-op
        // for every contact the approving device has already told.
        RosterGossipSender.announceIfOwed(store, identity, now())

        // The other half of the smoke script's converge gate: same head, both
        // phones, written down by each of them independently.
        Log.i(
            TAG,
            "link complete: role=approvingDevice deviceId=${hex(ack.deviceId)} " +
                "rosterHead=${hex(update.rosterHead)} devices=${update.roster.devices.size}",
        )

        _state.value = _state.value.copy(
            step = LinkStep.DONE,
            report = LinkReport(
                deviceIdHex = hex(ack.deviceId),
                rosterHeadHex = hex(update.rosterHead),
                rosterSeq = update.roster.seq,
                contacts = 0u,
                groups = 0u,
                messages = 0u,
                catchUpChats = 0,
            ),
        )
    }

    /**
     * This person's own roster, minting §3's genesis the first time.
     *
     * The genesis roster is root-signed, because at `seq == 0` there is nothing
     * else yet to sign it -- this is the identity upgrade §2 asks for, where the
     * deployed Ed25519 key becomes the person root and the phone holding it
     * becomes device one. It happens here, lazily, because WP3 is where the
     * first roster is first needed; the migration proper is WP6's to place.
     */
    private fun ownRoster(device: DeviceKeypair): Roster =
        store.ownRoster() ?: coreLinkGenesisRoster(
            identity.signSk,
            device.signPk,
            device.agreePk,
        ).also {
            store.adoptOwnRoster(it, identity.signPk, device.deviceId)
            rememberRosterSeen(it)
        }

    // -----------------------------------------------------------------------
    // Shared plumbing
    // -----------------------------------------------------------------------

    private fun drive(machine: LinkMachine, wire: LinkWire): CoreLinkSummary {
        val driver = LinkCeremonyDriver(
            machine = machine,
            wire = wire,
            clock = ::now,
            sleep = ::pause,
            observer = object : LinkCeremonyObserver {
                override fun onQr(qrText: String) {
                    _state.value = _state.value.copy(qrText = qrText)
                }

                override fun onSas(sas: String, confirmHere: Boolean, warnSoftCap: Boolean) {
                    _state.value = _state.value.copy(
                        step = LinkStep.COMPARING_DIGITS,
                        sas = sas,
                        confirmHere = confirmHere,
                        warnSoftCap = warnSoftCap,
                    )
                }

                override fun onProgress(sent: Boolean) {
                    if (sent && _state.value.step == LinkStep.WAITING_FOR_PEER) {
                        _state.value = _state.value.copy(step = LinkStep.HANDSHAKING)
                    }
                }
            },
            confirmDecision = { decision },
            cancelled = { cancelRequested },
        )
        val summary = driver.run()
        _state.value = _state.value.copy(
            outcome = summary.outcome,
            step = if (summary.outcome == CoreLinkOutcome.CHANNEL_READY) {
                LinkStep.CARRYING_BOOTSTRAP
            } else {
                LinkStep.FAILED
            },
        )
        return summary
    }

    private fun awaitFrame(wire: LinkWire): ByteArray {
        val deadline = now() + BOOTSTRAP_DEADLINE_MS
        while (now() < deadline) {
            if (cancelRequested) throw IOException("cancelled while waiting for the other device")
            wire.receive(RECEIVE_WAIT_MS)?.let { return it }
        }
        throw IOException("the other device stopped answering")
    }

    private fun requireRelay() = RelayConfigStore.load(appContext)
        ?: throw IOException("this device has no relay pass, so it cannot use a relay rendezvous")

    private fun now(): Long = System.currentTimeMillis()

    private fun pause(ms: Long) {
        if (ms > 0) Thread.sleep(ms)
    }

    /**
     * Stamp this phone's private "seen here since" note for every device in a
     * roster it has just adopted.
     *
     * At adoption, never at render: stamping while drawing the list meant "first
     * seen" was really "first looked at", and merely opening Settings wrote to
     * disk. A device with no stamp shows no date, which is the honest answer for
     * one this phone learned about before it kept notes.
     */
    private fun rememberRosterSeen(roster: Roster) {
        DeviceNameStore.rememberRoster(
            appContext,
            coreRosterDeviceIds(roster).map(::hex),
            now(),
        )
    }

    private fun hex(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it) }

    private companion object {
        const val TAG = "LinkSession"
        const val RECEIVE_WAIT_MS = 1_000L
        const val CONNECT_TIMEOUT_MS = 5_000

        /**
         * How long the §9.3-§9.4 exchange may take after the digits matched.
         * Separate from the ceremony's own deadline, which covers the part a
         * person is standing in front of.
         */
        const val BOOTSTRAP_DEADLINE_MS = 120_000L

        /**
         * How long to wait for the mesh service to actually take the radios
         * down after the §9.4 window opens. One handler post, so this is
         * generous; failing it aborts the ceremony rather than proceeding on a
         * phone that is still advertising.
         */
        const val VISIBILITY_APPLY_TIMEOUT_MS = 5_000L
    }
}
