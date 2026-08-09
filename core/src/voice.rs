//! Voice-message policy shared by both shells (`specs/voice-messages.md`).
//!
//! A voice message is an ordinary `AttachmentMediaType::Audio` attachment: it
//! seals, fragments, carries, relays, and is receipt-covered like a photo.
//! Nothing here touches the wire. What lives here is the part that used to be
//! duplicated (and disagreed) between Android and iOS: how long a recording may
//! run before it stops fitting in one envelope, and what the hold-to-talk
//! gesture means.

use crate::content::ATTACHMENT_MAX_BLOB_BYTES;

/// Capture sample rate. Speech only; 16 kHz is the standard wideband-voice rate
/// and halves the bytes of 32 kHz for no intelligibility loss on a phone mic.
const VOICE_SAMPLE_RATE_HZ: u32 = 16_000;

/// Encoder bitrate. See [`voice_capture_plan`] for the arithmetic that ties
/// this to the blob cap.
const VOICE_BITRATE_BPS: u32 = 20_000;

/// AAC-LC in an MPEG-4 container. Both shells' platform encoders produce this
/// natively (Android `MediaRecorder` MPEG_4/AAC, iOS `AVAudioRecorder`
/// `kAudioFormatMPEG4AAC`) and both platform decoders read the other's output,
/// which is why the container is the same on both sides and why already-shipped
/// clients keep playing new recordings.
const VOICE_MIME_TYPE: &str = "audio/mp4";

/// MPEG-4 framing that is *not* audio payload: `ftyp`, and a `moov` whose
/// sample tables grow with the frame count (~2,800 frames for a 60 s 16 kHz
/// AAC track). 8 KiB is a deliberately generous allowance — being wrong here in
/// the safe direction only costs recording seconds, being wrong in the other
/// direction costs the user a recording they already spoke.
const VOICE_CONTAINER_OVERHEAD_BYTES: u32 = 8 * 1024;

/// Reserve against a nominally-CBR encoder overshooting its target bitrate.
const VOICE_HEADROOM_PERCENT: u32 = 10;

/// UX ceiling on a single burst, independent of the byte budget. Push-to-talk
/// is "come to dinner", not a voicemail; a minute is already generous.
const VOICE_MAX_DURATION_CEILING_MS: u32 = 60_000;

/// Shorter than this is a pocket press or a mis-tap, not speech: discard it
/// rather than sending a click.
const VOICE_MIN_DURATION_MS: u32 = 700;

/// Slide-to-cancel threshold, in density-independent units (Android `dp`, iOS
/// points — the two differ by under 2% at any screen density).
const VOICE_CANCEL_SLIDE_DP: f32 = 72.0;

/// Slide-up-to-lock threshold, in the same units.
const VOICE_LOCK_SLIDE_DP: f32 = 64.0;

/// How the shells should configure the recorder, and the bounds they must
/// enforce around it.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct CoreVoiceCapturePlan {
    pub sample_rate_hz: u32,
    pub bitrate_bps: u32,
    pub mime_type: String,
    /// Longest recording that still fits one attachment envelope, with margin.
    pub max_duration_ms: u32,
    /// Shorter recordings are discarded as accidental.
    pub min_duration_ms: u32,
    /// Horizontal slide (toward the start edge) that arms cancel.
    pub cancel_slide_dp: f32,
    /// Upward slide that arms hands-free lock.
    pub lock_slide_dp: f32,
}

/// The single source of truth for recorder configuration on both shells.
///
/// The duration bound is *derived*, not asserted, so the two numbers cannot
/// drift apart the way they did before this existed — Android and iOS both
/// recorded 60 s at 32 kbps, which is 240 KB, and every recording past ~46 s
/// was rejected as too large *after* the user had already spoken it.
///
/// The arithmetic:
///
/// ```text
/// usable  = (cap - container_overhead) * (100 - headroom%) / 100
///         = (184320 - 8192) * 90 / 100            = 158,515 bytes
/// max_ms  = usable * 8 * 1000 / bitrate
///         = 158515 * 8000 / 20000                 =  63,406 ms
/// bound   = min(max_ms, ceiling)                  =  60,000 ms
/// ```
///
/// The ceiling binds, not the cap, which is the configuration we want: a
/// full-length 60 s burst is ~158 KB of the 180 KiB budget. If the bitrate ever
/// rises far enough for the cap to bind instead, the recording time shortens on
/// its own.
///
/// All of that assumes the encoder honours the bitrate. When it does not, the
/// clock is the wrong bound entirely, which is what
/// [`voice_capture_byte_budget`] and [`voice_capture_bytes`] are for.
#[uniffi::export]
pub fn voice_capture_plan() -> CoreVoiceCapturePlan {
    let usable = voice_capture_byte_budget() as u64;
    let budget_ms = usable * 8 * 1000 / VOICE_BITRATE_BPS as u64;
    let max_duration_ms = budget_ms.min(VOICE_MAX_DURATION_CEILING_MS as u64) as u32;
    CoreVoiceCapturePlan {
        sample_rate_hz: VOICE_SAMPLE_RATE_HZ,
        bitrate_bps: VOICE_BITRATE_BPS,
        mime_type: VOICE_MIME_TYPE.to_string(),
        max_duration_ms,
        min_duration_ms: VOICE_MIN_DURATION_MS,
        cancel_slide_dp: VOICE_CANCEL_SLIDE_DP,
        lock_slide_dp: VOICE_LOCK_SLIDE_DP,
    }
}

/// How many bytes of encoded audio a recording may accumulate on disk before it
/// has to stop, whatever the clock says.
///
/// The duration bound above trusts the encoder to honour the bitrate it was
/// asked for. Encoders are free not to: AAC implementations vary in how low a
/// bitrate they will accept at 16 kHz mono, and a device that quietly clamps
/// 20 kbps up to 24 or 32 produces a file over the cap while there is still
/// time on the clock. That failure lands on the user at the *worst* moment —
/// after they have spoken, with the recording already gone.
///
/// So the shells weigh the growing file on the same tick that advances the
/// clock, and whichever bound arrives first ends the recording and sends what
/// was said. Being byte-bound costs seconds; being unbound costs the message.
///
/// The budget is the payload room the duration arithmetic already reserves:
/// `(cap - container_overhead) * (100 - headroom%) / 100`. Measuring an
/// in-progress MPEG-4 file understates the finished size, because the `moov`
/// sample tables are written at stop — that is precisely what the container
/// allowance is holding back.
fn voice_capture_byte_budget() -> u32 {
    let cap = ATTACHMENT_MAX_BLOB_BYTES as u64;
    let overhead = VOICE_CONTAINER_OVERHEAD_BYTES as u64;
    (cap.saturating_sub(overhead) * (100 - VOICE_HEADROOM_PERCENT as u64) / 100)
        .min(u32::MAX as u64) as u32
}

/// Where a hold-to-talk gesture currently stands.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceCapturePhase {
    /// Not recording.
    Idle,
    /// Recording while the finger is down.
    Holding,
    /// Recording hands-free after a slide-up lock; the finger has let go.
    Locked,
}

/// What the shell must do as a result of the last gesture event.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceCaptureEffect {
    /// Nothing to do beyond re-rendering the state.
    None,
    /// Start the recorder.
    Start,
    /// Stop the recorder and send what was captured.
    Send,
    /// Stop the recorder and throw the audio away: too short to be speech.
    DiscardTooShort,
    /// Stop the recorder and throw the audio away: the user cancelled.
    DiscardCancelled,
}

/// The gesture's whole state. A record rather than an object so both shells can
/// hold it in their own idiomatic state container (Compose `mutableStateOf`,
/// SwiftUI `@State`) and reduce it with the free functions below.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreVoiceCaptureState {
    pub phase: VoiceCapturePhase,
    /// Releasing now would cancel rather than send.
    pub cancel_armed: bool,
    /// Releasing now would switch to hands-free rather than send.
    pub lock_armed: bool,
    /// Elapsed recording time the shell last reported, for the on-screen pip.
    pub elapsed_ms: u32,
}

/// A reduced state plus the side effect it asks the shell to perform.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreVoiceCaptureStep {
    pub state: CoreVoiceCaptureState,
    pub effect: VoiceCaptureEffect,
}

#[uniffi::export]
pub fn voice_capture_idle_state() -> CoreVoiceCaptureState {
    CoreVoiceCaptureState {
        phase: VoiceCapturePhase::Idle,
        cancel_armed: false,
        lock_armed: false,
        elapsed_ms: 0,
    }
}

/// Finger down on the mic button.
#[uniffi::export]
pub fn voice_capture_press(state: CoreVoiceCaptureState) -> CoreVoiceCaptureStep {
    if state.phase != VoiceCapturePhase::Idle {
        return unchanged(state);
    }
    step(
        CoreVoiceCaptureState {
            phase: VoiceCapturePhase::Holding,
            cancel_armed: false,
            lock_armed: false,
            elapsed_ms: 0,
        },
        VoiceCaptureEffect::Start,
    )
}

/// Finger moved `dx`/`dy` from where it went down, in dp/points, screen
/// coordinates (left and up are negative).
#[uniffi::export]
pub fn voice_capture_drag(state: CoreVoiceCaptureState, dx: f32, dy: f32) -> CoreVoiceCaptureStep {
    if state.phase != VoiceCapturePhase::Holding {
        return unchanged(state);
    }
    // Cancel wins a diagonal drag: sending something the user was trying to
    // throw away is the worse mistake of the two.
    let cancel_armed = dx <= -VOICE_CANCEL_SLIDE_DP;
    let lock_armed = !cancel_armed && dy <= -VOICE_LOCK_SLIDE_DP;
    unchanged(CoreVoiceCaptureState {
        cancel_armed,
        lock_armed,
        ..state
    })
}

/// The shell's recording clock ticked. Reaching the duration bound sends what
/// was said rather than failing — clip, don't fail.
#[uniffi::export]
pub fn voice_capture_elapsed(
    state: CoreVoiceCaptureState,
    elapsed_ms: u32,
) -> CoreVoiceCaptureStep {
    if state.phase == VoiceCapturePhase::Idle {
        return unchanged(state);
    }
    let advanced = CoreVoiceCaptureState {
        elapsed_ms,
        ..state
    };
    if elapsed_ms >= voice_capture_plan().max_duration_ms {
        return step(
            CoreVoiceCaptureState {
                elapsed_ms,
                ..voice_capture_idle_state()
            },
            VoiceCaptureEffect::Send,
        );
    }
    unchanged(advanced)
}

/// The shell weighed the file the encoder is writing. Running out of byte
/// budget ends the recording the same way running out of clock does: send what
/// was said. See [`voice_capture_byte_budget`] for why this exists at all.
#[uniffi::export]
pub fn voice_capture_bytes(
    state: CoreVoiceCaptureState,
    bytes_written: u32,
) -> CoreVoiceCaptureStep {
    if state.phase == VoiceCapturePhase::Idle {
        return unchanged(state);
    }
    if bytes_written >= voice_capture_byte_budget() {
        return step(
            CoreVoiceCaptureState {
                elapsed_ms: state.elapsed_ms,
                ..voice_capture_idle_state()
            },
            VoiceCaptureEffect::Send,
        );
    }
    unchanged(state)
}

/// Begin recording hands-free, with no hold at all.
///
/// Hold-to-talk is a gesture some people cannot make: a switch-access user has
/// no way to express "press and keep pressing", and a screen reader owns the
/// double-tap-and-hold that would otherwise reach the button. This is the same
/// state a slide-up lock reaches, entered directly, so those users get the
/// ordinary Cancel / Stop-and-send controls instead of a gesture.
#[uniffi::export]
pub fn voice_capture_start_hands_free(state: CoreVoiceCaptureState) -> CoreVoiceCaptureStep {
    if state.phase != VoiceCapturePhase::Idle {
        return unchanged(state);
    }
    step(
        CoreVoiceCaptureState {
            phase: VoiceCapturePhase::Locked,
            cancel_armed: false,
            lock_armed: false,
            elapsed_ms: 0,
        },
        VoiceCaptureEffect::Start,
    )
}

/// Finger lifted. `elapsed_ms` is the true held duration, which may be shorter
/// than any tick ever reported for a quick tap.
#[uniffi::export]
pub fn voice_capture_release(
    state: CoreVoiceCaptureState,
    elapsed_ms: u32,
) -> CoreVoiceCaptureStep {
    if state.phase != VoiceCapturePhase::Holding {
        // Already locked (or never started): lifting the finger changes nothing.
        return unchanged(state);
    }
    if state.cancel_armed {
        return step(
            voice_capture_idle_state(),
            VoiceCaptureEffect::DiscardCancelled,
        );
    }
    if state.lock_armed {
        return unchanged(CoreVoiceCaptureState {
            phase: VoiceCapturePhase::Locked,
            cancel_armed: false,
            lock_armed: false,
            elapsed_ms,
        });
    }
    finish(elapsed_ms)
}

/// The hands-free "Stop and send" control.
#[uniffi::export]
pub fn voice_capture_finish(state: CoreVoiceCaptureState, elapsed_ms: u32) -> CoreVoiceCaptureStep {
    if state.phase == VoiceCapturePhase::Idle {
        return unchanged(state);
    }
    finish(elapsed_ms)
}

/// Explicit cancel: the cancel control, a failed recorder, or leaving the chat.
#[uniffi::export]
pub fn voice_capture_cancel(state: CoreVoiceCaptureState) -> CoreVoiceCaptureStep {
    if state.phase == VoiceCapturePhase::Idle {
        return unchanged(state);
    }
    step(
        voice_capture_idle_state(),
        VoiceCaptureEffect::DiscardCancelled,
    )
}

fn finish(elapsed_ms: u32) -> CoreVoiceCaptureStep {
    let effect = if elapsed_ms < VOICE_MIN_DURATION_MS {
        VoiceCaptureEffect::DiscardTooShort
    } else {
        VoiceCaptureEffect::Send
    };
    step(voice_capture_idle_state(), effect)
}

fn step(state: CoreVoiceCaptureState, effect: VoiceCaptureEffect) -> CoreVoiceCaptureStep {
    CoreVoiceCaptureStep { state, effect }
}

fn unchanged(state: CoreVoiceCaptureState) -> CoreVoiceCaptureStep {
    CoreVoiceCaptureStep {
        state,
        effect: VoiceCaptureEffect::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a recording of `duration_ms` is *expected* to weigh if the encoder
    /// honours the bitrate. Only a test lives on this estimate; the shells act
    /// on the bytes the encoder actually wrote.
    fn estimated_blob_bytes(duration_ms: u32) -> u32 {
        let bytes_per_second = VOICE_BITRATE_BPS as u64 / 8;
        let payload = duration_ms as u64 * bytes_per_second / 1000;
        payload
            .saturating_add(VOICE_CONTAINER_OVERHEAD_BYTES as u64)
            .min(u32::MAX as u64) as u32
    }

    #[test]
    fn a_full_length_recording_fits_the_attachment_cap() {
        let plan = voice_capture_plan();
        let worst_case = estimated_blob_bytes(plan.max_duration_ms) as usize;
        assert!(
            worst_case <= ATTACHMENT_MAX_BLOB_BYTES,
            "a {} ms recording at {} bps is ~{worst_case} bytes, over the {ATTACHMENT_MAX_BLOB_BYTES} byte cap",
            plan.max_duration_ms,
            plan.bitrate_bps,
        );
        // And with real room to spare, not by a hair: an encoder that overshoots
        // its target must not push a legal recording over the cap.
        assert!(worst_case * 100 / ATTACHMENT_MAX_BLOB_BYTES <= 90);
    }

    #[test]
    fn the_plan_is_a_usable_walkie_talkie_burst() {
        let plan = voice_capture_plan();
        assert_eq!(plan.max_duration_ms, 60_000);
        assert_eq!(plan.min_duration_ms, 700);
        assert_eq!(plan.mime_type, "audio/mp4");
        assert_eq!(plan.sample_rate_hz, 16_000);
    }

    #[test]
    fn estimated_bytes_grow_with_duration_and_include_the_container() {
        assert_eq!(estimated_blob_bytes(0), 8 * 1024);
        assert_eq!(estimated_blob_bytes(1_000), 8 * 1024 + 2_500);
        assert!(estimated_blob_bytes(u32::MAX) > estimated_blob_bytes(60_000));
        assert!(estimated_blob_bytes(u32::MAX) as usize > ATTACHMENT_MAX_BLOB_BYTES);
    }

    #[test]
    fn the_byte_budget_bounds_a_recording_an_encoder_overshoots() {
        let budget = voice_capture_byte_budget() as usize;
        // Whatever else it is, it is a number a finished file can sit under.
        assert!(budget + VOICE_CONTAINER_OVERHEAD_BYTES as usize <= ATTACHMENT_MAX_BLOB_BYTES);

        let holding = voice_capture_press(voice_capture_idle_state()).state;
        let under = voice_capture_bytes(holding, budget as u32 - 1);
        assert_eq!(under.effect, VoiceCaptureEffect::None);
        assert_eq!(under.state.phase, VoiceCapturePhase::Holding);

        let over = voice_capture_bytes(holding, budget as u32);
        assert_eq!(over.effect, VoiceCaptureEffect::Send);
        assert_eq!(over.state.phase, VoiceCapturePhase::Idle);

        // The one encoder failure this exists for: a device that clamps the
        // requested 20 kbps up to 32 fills the budget at ~40 s, and the
        // recording ends there instead of being rejected after the fact.
        let bytes_at_40s = 40 * 32_000 / 8;
        assert!(bytes_at_40s > budget as u32);
        assert_eq!(
            voice_capture_bytes(holding, bytes_at_40s).effect,
            VoiceCaptureEffect::Send
        );

        // A locked recording is weighed the same way; an idle one is not.
        let locked = voice_capture_start_hands_free(voice_capture_idle_state()).state;
        assert_eq!(
            voice_capture_bytes(locked, budget as u32).effect,
            VoiceCaptureEffect::Send
        );
        assert_eq!(
            voice_capture_bytes(voice_capture_idle_state(), u32::MAX).effect,
            VoiceCaptureEffect::None
        );
    }

    #[test]
    fn hands_free_can_be_started_without_a_hold() {
        let started = voice_capture_start_hands_free(voice_capture_idle_state());
        assert_eq!(started.effect, VoiceCaptureEffect::Start);
        assert_eq!(started.state.phase, VoiceCapturePhase::Locked);

        // Lifting a finger that was never down must not send.
        assert_eq!(
            voice_capture_release(started.state, 5_000).effect,
            VoiceCaptureEffect::None
        );
        // Starting twice must not restart the recorder under the first one.
        assert_eq!(
            voice_capture_start_hands_free(started.state).effect,
            VoiceCaptureEffect::None
        );
        assert_eq!(
            voice_capture_finish(started.state, 5_000).effect,
            VoiceCaptureEffect::Send
        );
        // And the accidental-tap floor still applies on this path.
        assert_eq!(
            voice_capture_finish(started.state, 100).effect,
            VoiceCaptureEffect::DiscardTooShort
        );
    }

    #[test]
    fn hold_and_release_sends() {
        let pressed = voice_capture_press(voice_capture_idle_state());
        assert_eq!(pressed.effect, VoiceCaptureEffect::Start);
        assert_eq!(pressed.state.phase, VoiceCapturePhase::Holding);

        let released = voice_capture_release(pressed.state, 3_000);
        assert_eq!(released.effect, VoiceCaptureEffect::Send);
        assert_eq!(released.state, voice_capture_idle_state());
    }

    #[test]
    fn a_tap_too_short_to_be_speech_is_discarded() {
        let pressed = voice_capture_press(voice_capture_idle_state());
        let released = voice_capture_release(pressed.state, 300);
        assert_eq!(released.effect, VoiceCaptureEffect::DiscardTooShort);
        assert_eq!(released.state.phase, VoiceCapturePhase::Idle);
    }

    #[test]
    fn sliding_left_arms_cancel_and_releasing_discards() {
        let plan = voice_capture_plan();
        let pressed = voice_capture_press(voice_capture_idle_state());
        let nudged = voice_capture_drag(pressed.state, -(plan.cancel_slide_dp - 1.0), 0.0);
        assert!(!nudged.state.cancel_armed);

        let armed = voice_capture_drag(nudged.state, -(plan.cancel_slide_dp + 1.0), 0.0);
        assert!(armed.state.cancel_armed);
        assert_eq!(armed.effect, VoiceCaptureEffect::None);

        let released = voice_capture_release(armed.state, 9_000);
        assert_eq!(released.effect, VoiceCaptureEffect::DiscardCancelled);
    }

    #[test]
    fn sliding_back_from_cancel_disarms_it() {
        let pressed = voice_capture_press(voice_capture_idle_state());
        let armed = voice_capture_drag(pressed.state, -200.0, 0.0);
        assert!(armed.state.cancel_armed);
        let disarmed = voice_capture_drag(armed.state, -4.0, 0.0);
        assert!(!disarmed.state.cancel_armed);
        assert_eq!(
            voice_capture_release(disarmed.state, 2_000).effect,
            VoiceCaptureEffect::Send
        );
    }

    #[test]
    fn sliding_up_locks_hands_free_and_keeps_recording() {
        let plan = voice_capture_plan();
        let pressed = voice_capture_press(voice_capture_idle_state());
        let armed = voice_capture_drag(pressed.state, 0.0, -(plan.lock_slide_dp + 1.0));
        assert!(armed.state.lock_armed);

        let released = voice_capture_release(armed.state, 2_000);
        assert_eq!(released.effect, VoiceCaptureEffect::None);
        assert_eq!(released.state.phase, VoiceCapturePhase::Locked);
        assert!(!released.state.lock_armed);

        // A second finger-lift while locked must not send twice.
        assert_eq!(
            voice_capture_release(released.state, 3_000).effect,
            VoiceCaptureEffect::None
        );

        let stopped = voice_capture_finish(released.state, 3_000);
        assert_eq!(stopped.effect, VoiceCaptureEffect::Send);
        assert_eq!(stopped.state.phase, VoiceCapturePhase::Idle);
    }

    #[test]
    fn a_diagonal_drag_prefers_cancel_over_lock() {
        let pressed = voice_capture_press(voice_capture_idle_state());
        let dragged = voice_capture_drag(pressed.state, -120.0, -120.0);
        assert!(dragged.state.cancel_armed);
        assert!(!dragged.state.lock_armed);
    }

    #[test]
    fn reaching_the_duration_bound_sends_what_was_said() {
        let plan = voice_capture_plan();
        let pressed = voice_capture_press(voice_capture_idle_state());
        let ticked = voice_capture_elapsed(pressed.state, 1_000);
        assert_eq!(ticked.effect, VoiceCaptureEffect::None);
        assert_eq!(ticked.state.elapsed_ms, 1_000);

        let capped = voice_capture_elapsed(ticked.state, plan.max_duration_ms);
        assert_eq!(capped.effect, VoiceCaptureEffect::Send);
        assert_eq!(capped.state.phase, VoiceCapturePhase::Idle);
    }

    #[test]
    fn a_locked_recording_also_stops_at_the_duration_bound() {
        let plan = voice_capture_plan();
        let locked = voice_capture_release(
            voice_capture_drag(
                voice_capture_press(voice_capture_idle_state()).state,
                0.0,
                -200.0,
            )
            .state,
            1_000,
        );
        assert_eq!(locked.state.phase, VoiceCapturePhase::Locked);
        let capped = voice_capture_elapsed(locked.state, plan.max_duration_ms + 500);
        assert_eq!(capped.effect, VoiceCaptureEffect::Send);
    }

    #[test]
    fn cancelling_from_any_recording_phase_discards() {
        for state in [
            voice_capture_press(voice_capture_idle_state()).state,
            voice_capture_release(
                voice_capture_drag(
                    voice_capture_press(voice_capture_idle_state()).state,
                    0.0,
                    -200.0,
                )
                .state,
                1_000,
            )
            .state,
        ] {
            let cancelled = voice_capture_cancel(state);
            assert_eq!(cancelled.effect, VoiceCaptureEffect::DiscardCancelled);
            assert_eq!(cancelled.state, voice_capture_idle_state());
        }
    }

    #[test]
    fn events_outside_a_recording_do_nothing() {
        let idle = voice_capture_idle_state();
        for stepped in [
            voice_capture_drag(idle, -500.0, -500.0),
            voice_capture_elapsed(idle, 90_000),
            voice_capture_release(idle, 90_000),
            voice_capture_finish(idle, 90_000),
            voice_capture_cancel(idle),
        ] {
            assert_eq!(stepped.effect, VoiceCaptureEffect::None);
            assert_eq!(stepped.state, idle);
        }

        // A second press while already recording must not restart the recorder.
        let holding = voice_capture_press(idle).state;
        assert_eq!(
            voice_capture_press(holding).effect,
            VoiceCaptureEffect::None
        );
    }
}
