// Talkback push-to-talk: capture mic, encode Opus, send via WebSocket (reaperiem#123)

// The session of the last start: its parts live on it, never in shared
// variables, so a stop while a start still waits frees what that start made
// and a start that resumes late never touches a newer session (live run of
// 819f954, 2026-10-23: a release during getUserMedia left the microphone open,
// the session "active" with no socket, and every later press returned at once).
let _session = null;

/** A session's parts are freed once; a stopped start resolves without an error. */
function freeSession(s) {
  s.stopped = true;
  if (s.processorNode) {
    s.processorNode.port.onmessage = null;
    s.processorNode.disconnect();
    s.processorNode = null;
  }
  if (s.sourceNode) {
    s.sourceNode.disconnect();
    s.sourceNode = null;
  }
  if (s.encoder && s.encoder.state !== 'closed') {
    try { s.encoder.close(); } catch (_) {}
  }
  s.encoder = null;
  if (s.stream) {
    s.stream.getTracks().forEach((t) => t.stop());
    s.stream = null;
  }
  if (s.audioCtx) {
    s.audioCtx.close().catch(() => {});
    s.audioCtx = null;
  }
  if (s.ws) {
    const ws = s.ws;
    if (ws.readyState === WebSocket.OPEN) {
      ws.close();
    } else if (ws.readyState === WebSocket.CONNECTING) {
      // Closing a socket that still connects makes Chrome log "WebSocket is
      // closed before the connection is established": close it once it opens.
      ws.onopen = () => ws.close();
    }
    s.ws = null;
  }
}

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
 * A stop while it waits ends it quietly: it frees what it made and resolves.
 * @param {string} wsUrl - WebSocket URL for talkback audio (wss://host/ws/talkback?token=...&talk=<id>)
 * @returns {Promise<void>}
 */
export async function startTalkback(wsUrl) {
  if (_session && !_session.stopped) return;
  const s = { state: 'connecting', stopped: false };
  _session = s;

  try {
    // 1. Open the dedicated talkback WebSocket first: it binds the held talk
    //    lock by the talk id in its URL, which the server allows only briefly
    //    after the lock was granted (X6).
    const ws = new WebSocket(wsUrl);
    s.ws = ws;
    ws.binaryType = 'arraybuffer';

    await new Promise((resolve, reject) => {
      ws.onopen = resolve;
      ws.onerror = () => reject(new Error('Talkback WS failed to connect'));
      // A stop closes the socket while it connects: that is no failure.
      ws.onclose = () => (s.stopped ? resolve() : reject(new Error('Talkback WS closed while connecting')));
      // Timeout after 5s
      setTimeout(() => reject(new Error('Talkback WS connect timeout')), 5000);
    });
    if (s.stopped) return;

    // 2. Get mic stream (mono, 48kHz, echo/noise cancellation)
    const stream = await navigator.mediaDevices.getUserMedia({
      audio: {
        channelCount: 1,
        sampleRate: 48000,
        echoCancellation: true,
        noiseSuppression: true,
        autoGainControl: true,
      },
      video: false,
    });
    s.stream = stream;
    if (s.stopped) {
      freeSession(s);
      return;
    }

    // 3. AudioContext at 48kHz
    const audioCtx = new AudioContext({ sampleRate: 48000 });
    s.audioCtx = audioCtx;

    // 4. WebCodecs AudioEncoder (Opus, 96 kbps mono — reaperiem#154 quality bump).
    const encoder = new AudioEncoder({
      output: (chunk) => {
        if (s.ws && s.ws.readyState === WebSocket.OPEN) {
          const buf = new ArrayBuffer(chunk.byteLength);
          chunk.copyTo(buf);
          s.ws.send(buf);
        }
      },
      error: (e) => {
        if (s.stopped) return;
        console.error('[talkback] encoder error:', e);
        stopTalkback();
      },
    });
    s.encoder = encoder;

    encoder.configure({
      codec: 'opus',
      sampleRate: 48000,
      numberOfChannels: 1,
      bitrate: 96000,
    });

    // 5. AudioWorklet feeds the encoder with exact 960-sample (20 ms) frames.
    //    Replaces deprecated ScriptProcessorNode(1024) which caused Opus
    //    re-framing jitter and DOM/GC stalls on the main thread.
    try {
      await audioCtx.audioWorklet.addModule('/talkback-worklet.js');
    } catch (err) {
      if (s.stopped) return;
      console.error('[talkback] failed to load worklet module:', err);
      throw err;
    }
    if (s.stopped) {
      freeSession(s);
      return;
    }

    s.sourceNode = audioCtx.createMediaStreamSource(stream);
    s.processorNode = new AudioWorkletNode(audioCtx, 'talkback-worklet', {
      numberOfInputs: 1,
      numberOfOutputs: 1,
      outputChannelCount: [1],
    });

    s.processorNode.port.onmessage = (ev) => {
      if (!s.encoder || s.encoder.state !== 'configured') return;
      const data = ev.data; // Float32Array(960)
      const frame = new AudioData({
        format: 'f32-planar',
        sampleRate: 48000,
        numberOfFrames: data.length,
        numberOfChannels: 1,
        timestamp: (performance.now() * 1000) | 0, // microseconds
        data: data,
      });
      s.encoder.encode(frame);
      frame.close();
    };

    s.sourceNode.connect(s.processorNode);
    // Worklet output is unused — connect to a muted gain so the graph is
    // fully wired (required on older Safari; harmless elsewhere).
    const silentGain = audioCtx.createGain();
    silentGain.gain.value = 0;
    s.processorNode.connect(silentGain);
    silentGain.connect(audioCtx.destination);
    s.state = 'active';
    console.log('[talkback] active');
  } catch (err) {
    // A start the stop already ended is no failure.
    if (s.stopped) {
      freeSession(s);
      return;
    }
    console.error('[talkback] start failed:', err);
    freeSession(s);
    throw err;
  }
}

/**
 * Stop talkback and clean up all resources, also of a start still waiting.
 */
export function stopTalkback() {
  if (_session) {
    freeSession(_session);
    _session = null;
  }
  console.log('[talkback] stopped');
}

/**
 * Get current talkback state.
 * @returns {'idle'|'connecting'|'active'}
 */
export function getTalkbackState() {
  return _session ? _session.state : 'idle';
}
