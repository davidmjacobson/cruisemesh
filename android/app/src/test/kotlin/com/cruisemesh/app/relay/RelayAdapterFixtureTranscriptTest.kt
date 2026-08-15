package com.cruisemesh.app.relay

import com.cruisemesh.app.mesh.CoreRelayPassRunner
import com.cruisemesh.app.mesh.HostCoreLibrary
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import okhttp3.mockwebserver.SocketPolicy
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.CoreRelayFixtureEndpoint
import uniffi.cruisemesh_core.CoreRelayFixtureObservedRequest
import uniffi.cruisemesh_core.CoreRelayFixtureReply
import uniffi.cruisemesh_core.CoreRelayFixtureTranscript
import uniffi.cruisemesh_core.CoreRelayHttpRequest
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.coreRelayFixtureExpectedTranscript
import uniffi.cruisemesh_core.coreRelayFixtureIdealObservation
import uniffi.cruisemesh_core.coreRelayFixtureNames
import uniffi.cruisemesh_core.coreRelayFixturePlan
import uniffi.cruisemesh_core.coreRelayFixtureReply
import uniffi.cruisemesh_core.coreRelayFixtureScenario
import uniffi.cruisemesh_core.coreRelayFixtureSeedStore
import uniffi.cruisemesh_core.coreRelayFixtureViolatedInvariants

/**
 * The incident corpus, executed through this platform's relay adapter.
 *
 * Until now the fixtures under `core/tests/fixtures/` were executed only in
 * Rust, and the only thing the two shells' adapters shared was a table of four
 * request shapes. A request shape says nothing about whether a whole incident
 * — several passes, one store, a relay that stops answering part-way — ends in
 * the same state once a real driver, a real socket and a real HTTP client are
 * in the loop.
 *
 * Each case here takes one fixture scenario from core, seeds a real
 * `MessageStore` from core, drives every pass through the *production*
 * [CoreRelayPassRunner] and [CoreRelayDriver] against a `MockWebServer`
 * answering core's script, and compares the normalised transcript against
 * `coreRelayFixtureExpectedTranscript` — the same scenario run in Rust with
 * nothing but the HTTP replaced.
 *
 * # What a failure here means
 *
 * The transcript carries, per pass, every request as *the server received it*,
 * how the driver reported each answer, the pass summary, and then the store
 * state, the emitted protocol-event codes and any invariant the session
 * reported violated. So a red here is one of: a driver that mangled a query
 * string, dropped a body, altered a header, swallowed or invented a status,
 * mislabelled a transport failure as an answer, or a runner that issued the
 * wrong number of actions. Every one of those is invisible to a per-request
 * vector test and is exactly what a migration produces.
 *
 * # Why the expectation lives in core
 *
 * Writing it down here and again in Swift would be three descriptions of one
 * behaviour, and the two hand-written ones would be written by reading the
 * first — so they would agree on its mistakes. The comparison is against a run
 * of the same scenario instead.
 *
 * # Scope
 *
 * The fixtures wired today are `carry-storm` and `contact-silence-no-proof`.
 * Adding another is one arm in core's scenario table; this file iterates
 * `coreRelayFixtureNames()` and needs no change. Fixtures whose transcripts
 * turn on group fan-out wait for the core upload lanes to decompose a
 * group-addressed row.
 */
class RelayAdapterFixtureTranscriptTest {

    companion object {
        init {
            HostCoreLibrary.load()
        }

        private const val OWN_TOKEN = "member-token-own"
        private const val CONTACT_TOKEN = "member-token-contact"
    }

    @Test
    fun `every wired fixture drives through the real adapter to the transcript core expects`() {
        val names = coreRelayFixtureNames()
        assertTrue("the corpus must wire at least one fixture", names.isNotEmpty())
        for (name in names) {
            assertEquals(
                "$name: the transcript this adapter produced differs from the reference run",
                coreRelayFixtureExpectedTranscript(name),
                driveThroughTheAdapter(name),
            )
        }
    }

    @Test
    fun `no fixture scenario reports one of its declared invariants violated`() {
        // The same claim `relay_pass_replay.rs` makes in Rust, made here about
        // a store a phone's own driver filled. It is redundant with the
        // transcript comparison above by construction and stated anyway: this
        // is the sentence a person reads when it goes red, and "the strings
        // differ" is not that sentence.
        for (name in coreRelayFixtureNames()) {
            val run = Run(name)
            try {
                run.drive()
                val violated = coreRelayFixtureViolatedInvariants(run.store)
                for (declared in coreRelayFixtureScenario(name).declaredInvariants) {
                    assertTrue(
                        "$name: the session reported $declared violated",
                        !violated.contains(declared),
                    )
                }
            } finally {
                run.shutdown()
            }
        }
    }

    private fun driveThroughTheAdapter(name: String): String {
        val run = Run(name)
        return try {
            run.drive()
        } finally {
            run.shutdown()
        }
    }

    /**
     * One scenario, standing up as much of the phone as a JVM can hold.
     *
     * Two servers rather than one because a contact's mailbox lives on its own
     * relay, and `SILENCE-01` is about telling "the contact is quiet" apart
     * from "this phone is offline" — a distinction that does not exist if both
     * endpoints are the same host.
     */
    private class Run(private val name: String) {

        private val scenario = coreRelayFixtureScenario(name)
        private val own = ScriptedRelay()
        private val contact = ScriptedRelay()
        val store: MessageStore = MessageStore.open(":memory:")

        init {
            own.start()
            if (scenario.usesContactEndpoint) contact.start()
            coreRelayFixtureSeedStore(store, name)
        }

        fun shutdown() {
            own.shutdown()
            if (scenario.usesContactEndpoint) contact.shutdown()
        }

        fun drive(): String {
            val transcript = CoreRelayFixtureTranscript(name)
            val ownBase = normalizeRelayUrl(own.baseUrl())
            val contactBase =
                if (scenario.usesContactEndpoint) normalizeRelayUrl(contact.baseUrl()) else ownBase

            for ((index, spec) in scenario.passes.withIndex()) {
                val passIndex = index.toUInt()
                CoreRelayPassRunner(
                    store = store,
                    executor = { passId, actionId, request, atMs ->
                        val endpoint = endpointOf(request, ownBase)
                        val relay = if (endpoint == CoreRelayFixtureEndpoint.OWN) own else contact
                        val reply = coreRelayFixtureReply(name, passIndex, request.operation, endpoint)
                        relay.answerNextWith(reply)

                        val result =
                            CoreRelayDriver.execute(passId, actionId, request, null, atMs)

                        transcript.recordRequest(
                            passIndex,
                            request,
                            endpoint,
                            relay.observation(request),
                        )
                        transcript.recordResult(passIndex, result)
                        result
                    },
                    clock = { spec.nowMs },
                ).run(
                    coreRelayFixturePlan(
                        name,
                        passIndex,
                        ownBase,
                        OWN_TOKEN,
                        contactBase,
                        CONTACT_TOKEN,
                    ),
                    spec.label,
                ).let { transcript.recordSummary(passIndex, spec, it) }
            }

            return transcript.finish(store, ownBase, OWN_TOKEN)
        }

        /**
         * Which configured endpoint this request is for.
         *
         * The one decision this harness makes, and it is an addressing one
         * rather than a protocol one: core chose the base URL, and the test
         * only has to recognise which of the two servers it stood up that is.
         */
        private fun endpointOf(request: CoreRelayHttpRequest, ownBase: String) =
            if (request.baseUrl == ownBase) {
                CoreRelayFixtureEndpoint.OWN
            } else {
                CoreRelayFixtureEndpoint.CONTACT
            }
    }

    /**
     * A relay that answers exactly what core's script says, and records what it
     * was asked.
     *
     * The recording is the point: the transcript's request lines come from the
     * bytes that reached the server, not from the [CoreRelayHttpRequest] core
     * formed, so a driver that alters one between the two is what this catches.
     * Everything is set immediately before the driver call on the same thread,
     * so the plain fields need no synchronisation.
     */
    private class ScriptedRelay {

        private val server = MockWebServer()
        private var next: CoreRelayFixtureReply? = null
        private var observed: CoreRelayFixtureObservedRequest? = null

        fun start() {
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse {
                    val reply = next ?: return MockResponse().setResponseCode(200).setBody("{}")
                    // First attempt only. A retry underneath the driver belongs
                    // to the HTTP client rather than to the protocol, and the
                    // request being compared is the one the pass asked for.
                    if (observed == null) {
                        observed = CoreRelayFixtureObservedRequest(
                            method = request.method.orEmpty(),
                            path = request.path.orEmpty(),
                            bodyLen = request.bodySize.toUInt(),
                            authorization = request.getHeader("Authorization"),
                        )
                    }
                    if (reply.transportFailure) {
                        // Read the request, then drop the connection without a
                        // status line: a relay that could not be reached at
                        // all, which is what `SILENCE-01` turns on.
                        return MockResponse().setSocketPolicy(SocketPolicy.DISCONNECT_AFTER_REQUEST)
                    }
                    var response = MockResponse().setResponseCode(reply.status.toInt())
                    for (header in reply.headers) response = response.setHeader(header.name, header.value)
                    return response.setBody(okio.Buffer().write(reply.body))
                }
            }
            server.start()
        }

        fun answerNextWith(reply: CoreRelayFixtureReply) {
            next = reply
            observed = null
        }

        /**
         * What the server saw, or core's own form of the request when the fake
         * transport refused it before reading a byte. There is nothing else to
         * compare in that case, and the alternative — dropping the line — would
         * make the two platforms' transcripts differ over which of them managed
         * to observe a request it was always going to fail.
         */
        fun observation(request: CoreRelayHttpRequest) =
            observed ?: coreRelayFixtureIdealObservation(request)

        fun baseUrl(): String = server.url("/").toString()

        fun shutdown() = server.shutdown()
    }
}
