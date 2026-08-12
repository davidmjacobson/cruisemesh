import { Button } from "@fluentui/react-components";
import { Pause24Filled, Play24Filled } from "@fluentui/react-icons";
import { PointerEvent, useSyncExternalStore } from "react";
import { formatDurationMs, userCopy, voiceProgress } from "./presentation";
import { voicePlayback } from "./voice";

export function VoicePlayer({
  messageKey,
  src,
  durationMs,
}: {
  messageKey: string;
  src: string;
  durationMs: number;
}) {
  const state = useSyncExternalStore(voicePlayback.subscribe, voicePlayback.snapshot);
  const active = state.key === messageKey;
  const playing = active && state.playing;
  const failed = active && state.failed;
  const totalMs = active && state.durationMs > 0 ? state.durationMs : durationMs;
  const positionMs = active ? state.positionMs : 0;
  const progress = voiceProgress(positionMs, totalMs);

  function seekFromPointer(event: PointerEvent<HTMLDivElement>) {
    const rect = event.currentTarget.getBoundingClientRect();
    if (rect.width <= 0) return;
    voicePlayback.prepare(messageKey, src, durationMs);
    voicePlayback.seek((event.clientX - rect.left) / rect.width);
  }

  return (
    <div className="voice-player">
      <Button
        appearance="subtle"
        icon={playing ? <Pause24Filled /> : <Play24Filled />}
        aria-label={playing ? userCopy.pauseVoiceMessage : userCopy.playVoiceMessage}
        onClick={() => voicePlayback.toggle(messageKey, src, durationMs)}
      />
      <div className="voice-track">
        <div className="voice-times">
          {formatDurationMs(positionMs)} / {formatDurationMs(totalMs)}
        </div>
        <div
          className="voice-bar"
          role="slider"
          tabIndex={0}
          aria-label="Voice message position"
          aria-valuemin={0}
          aria-valuemax={Math.max(1, totalMs)}
          aria-valuenow={positionMs}
          aria-valuetext={`${formatDurationMs(positionMs)} of ${formatDurationMs(totalMs)}`}
          onPointerDown={seekFromPointer}
          onKeyDown={(event) => {
            if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
            event.preventDefault();
            voicePlayback.prepare(messageKey, src, durationMs);
            const step = event.key === "ArrowRight" ? 0.08 : -0.08;
            voicePlayback.seek(progress + step);
          }}
        >
          <span className="voice-bar-fill" style={{ width: `${progress * 100}%` }} />
        </div>
        {failed && <div className="voice-error">{userCopy.voicePlaybackFailed}</div>}
      </div>
    </div>
  );
}
