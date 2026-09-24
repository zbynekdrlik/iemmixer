// Talkback push-to-talk: capture mic, encode Opus, send via WebSocket (reaperiem#123)

let _state = 'idle'; // 'idle' | 'connecting' | 'active'
let _stream = null;
let _audioCtx = null;
let _encoder = null;
let _sourceNode = null;
let _processorNode = null;
let _ws = null;

/**
 * Check if browser supports talkback (mediaDevices + AudioEncoder).
 * @returns {boolean}
 */
export function isTalkbackSupported() {
  return !!(
    navigator.mediaDevices &&
    navigator.mediaDevices.getUserMedia &&
    typeof AudioEncoder !== 'undefined'
  );
}

/**
 * Start talkback: capture mic, encode Opus, send binary frames via new WebSocket.
 * @param {string} wsUrl - WebSocket URL for talkback audio (e.g. wss://host/ws/talkback?token=...)
 * @returns {Promise<void>}
 */
export async function startTalkback(wsUrl) {
  if (_state === 'active') return;
  _state = 'connecting';

  try {
    // 1. Get mic stream (mono, 48kHz, echo/noise cancellation)
    _stream = await navigator.mediaDevices.getUserMedia({
      audio: {
        channelCount: 1,
        sampleRate: 48000,
        echoCancellation: true,
        noiseSuppression: true,
        autoGainControl: true,
      },
      video: false,
    });

    // 2. AudioContext at 48kHz
    _audioCtx = new AudioContext({ sampleRate: 48000 });

    // 3. Open dedicated talkback WebSocket
    _ws = new WebSocket(wsUrl);
    _ws.binaryType = 'arraybuffer';

    await new Promise((resolve, reject) => {
      _ws.onopen = resolve;
      _ws.onerror = () => reject(new Error('Talkback WS failed to connect'));
      // Timeout after 5s
      setTimeout(() => reject(new Error('Talkback WS connect timeout')), 5000);
    });

    // 4. WebCodecs AudioEncoder (Opus, 96 kbps mono — reaperiem#154 quality bump).
    _encoder = new AudioEncoder({
      output: (chunk) => {
        if (_ws && _ws.readyState === WebSocket.OPEN) {
          const buf = new ArrayBuffer(chunk.byteLength);
          chunk.copyTo(buf);
          _ws.send(buf);
        }
      },
      error: (e) => {
        console.error('[talkback] encoder error:', e);
        stopTalkback();
      },
    });

    _encoder.configure({
      codec: 'opus',
      sampleRate: 48000,
      numberOfChannels: 1,
      bitrate: 96000,
    });

    // 5. AudioWorklet feeds the encoder with exact 960-sample (20 ms) frames.
    //    Replaces deprecated ScriptProcessorNode(1024) which caused Opus
    //    re-framing jitter and DOM/GC stalls on the main thread.
    try {
      await _audioCtx.audioWorklet.addModule('/talkback-worklet.js');
    } catch (err) {
      console.error('[talkback] failed to load worklet module:', err);
      throw err;
    }

    _sourceNode = _audioCtx.createMediaStreamSource(_stream);
    _processorNode = new AudioWorkletNode(_audioCtx, 'talkback-worklet', {
      numberOfInputs: 1,
      numberOfOutputs: 1,
      outputChannelCount: [1],
    });

    _processorNode.port.onmessage = (ev) => {
      if (!_encoder || _encoder.state !== 'configured') return;
      const data = ev.data; // Float32Array(960)
      const frame = new AudioData({
        format: 'f32-planar',
        sampleRate: 48000,
        numberOfFrames: data.length,
        numberOfChannels: 1,
        timestamp: (performance.now() * 1000) | 0, // microseconds
        data: data,
      });
      _encoder.encode(frame);
      frame.close();
    };

    _sourceNode.connect(_processorNode);
    // Worklet output is unused — connect to a muted gain so the graph is
    // fully wired (required on older Safari; harmless elsewhere).
    const silentGain = _audioCtx.createGain();
    silentGain.gain.value = 0;
    _processorNode.connect(silentGain);
    silentGain.connect(_audioCtx.destination);
    _state = 'active';
    console.log('[talkback] active');
  } catch (err) {
    console.error('[talkback] start failed:', err);
    stopTalkback();
    throw err;
  }
}

/**
 * Stop talkback and clean up all resources.
 */
export function stopTalkback() {
  if (_processorNode) {
    _processorNode.disconnect();
    _processorNode.onaudioprocess = null;
    _processorNode = null;
  }
  if (_sourceNode) {
    _sourceNode.disconnect();
    _sourceNode = null;
  }
  if (_encoder && _encoder.state !== 'closed') {
    try { _encoder.close(); } catch (_) {}
    _encoder = null;
  }
  if (_stream) {
    _stream.getTracks().forEach((t) => t.stop());
    _stream = null;
  }
  if (_audioCtx) {
    _audioCtx.close().catch(() => {});
    _audioCtx = null;
  }
  if (_ws) {
    if (_ws.readyState === WebSocket.OPEN || _ws.readyState === WebSocket.CONNECTING) {
      _ws.close();
    }
    _ws = null;
  }
  _state = 'idle';
  console.log('[talkback] stopped');
}

/**
 * Get current talkback state.
 * @returns {'idle'|'connecting'|'active'}
 */
export function getTalkbackState() {
  return _state;
}
