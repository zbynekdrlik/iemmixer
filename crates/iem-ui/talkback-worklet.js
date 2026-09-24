// AudioWorklet processor for talkback (reaperiem#154).
// Accumulates exact 960-sample (20 ms @ 48 kHz) mono frames and posts
// each frame to the main thread. This replaces the deprecated
// ScriptProcessorNode(1024) path which caused Opus re-framing jitter.

class TalkbackWorklet extends AudioWorkletProcessor {
  constructor() {
    super();
    this._frame = new Float32Array(960);
    this._write = 0;
  }

  process(inputs) {
    const input = inputs[0];
    if (!input || input.length === 0) return true;
    const ch0 = input[0];
    if (!ch0) return true;

    for (let i = 0; i < ch0.length; i++) {
      this._frame[this._write++] = ch0[i];
      if (this._write === 960) {
        // Copy (transferable is fastest but the buffer is tiny).
        this.port.postMessage(this._frame.slice());
        this._write = 0;
      }
    }
    return true;
  }
}

registerProcessor("talkback-worklet", TalkbackWorklet);
