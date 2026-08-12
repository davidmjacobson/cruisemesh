export type VoicePlaybackState = {
  key: string | null;
  playing: boolean;
  positionMs: number;
  durationMs: number;
  failed: boolean;
};

const idle: VoicePlaybackState = {
  key: null,
  playing: false,
  positionMs: 0,
  durationMs: 0,
  failed: false,
};

type Listener = () => void;

class VoicePlayback {
  private audio: HTMLAudioElement | undefined;
  private listeners = new Set<Listener>();
  private state: VoicePlaybackState = idle;

  snapshot = (): VoicePlaybackState => this.state;

  subscribe = (listener: Listener): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  toggle(key: string, src: string, fallbackDurationMs: number) {
    const audio = this.ensureAudio();
    if (this.state.key === key) {
      if (this.state.playing) {
        audio.pause();
        this.set({ playing: false });
        return;
      }
      this.play(audio);
      return;
    }
    this.load(audio, key, src, fallbackDurationMs);
    this.play(audio);
  }

  prepare(key: string, src: string, fallbackDurationMs: number) {
    if (this.state.key === key) return;
    this.load(this.ensureAudio(), key, src, fallbackDurationMs);
  }

  seek(ratio: number) {
    const audio = this.audio;
    if (!audio || this.state.durationMs <= 0) return;
    const next = Math.min(1, Math.max(0, ratio)) * this.state.durationMs;
    audio.currentTime = next / 1000;
    this.set({ positionMs: next });
  }

  stop() {
    if (!this.audio) {
      this.set(idle);
      return;
    }
    this.audio.pause();
    this.audio.removeAttribute("src");
    this.audio.load();
    this.set(idle);
  }

  private ensureAudio(): HTMLAudioElement {
    if (this.audio) return this.audio;
    const audio = new Audio();
    audio.preload = "auto";
    audio.addEventListener("timeupdate", () => this.syncPosition());
    audio.addEventListener("durationchange", () => this.syncDuration());
    audio.addEventListener("ended", () => {
      this.set({ playing: false, positionMs: 0 });
    });
    audio.addEventListener("error", () => {
      this.set({ playing: false, failed: true });
    });
    this.audio = audio;
    return audio;
  }

  private load(audio: HTMLAudioElement, key: string, src: string, fallbackDurationMs: number) {
    audio.pause();
    audio.src = src;
    this.set({
      key,
      playing: false,
      positionMs: 0,
      durationMs: fallbackDurationMs,
      failed: false,
    });
  }

  private play(audio: HTMLAudioElement) {
    void audio.play().then(
      () => this.set({ playing: true, failed: false }),
      () => this.set({ playing: false, failed: true }),
    );
  }

  private syncPosition() {
    if (!this.audio) return;
    this.set({ positionMs: Math.round(this.audio.currentTime * 1000) });
  }

  private syncDuration() {
    if (!this.audio) return;
    const seconds = this.audio.duration;
    if (Number.isFinite(seconds) && seconds > 0) {
      this.set({ durationMs: Math.round(seconds * 1000) });
    }
  }

  private set(partial: Partial<VoicePlaybackState>) {
    this.state = { ...this.state, ...partial };
    for (const listener of this.listeners) listener();
  }
}

export const voicePlayback = new VoicePlayback();
