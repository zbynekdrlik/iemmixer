//! The listen probe through the pipes (S7 design note §6, #10): NullRt under
//! `--test-signal` on the test site (`[guard] hil_tx` 94/95). While a HIL
//! signal with `listen` runs, the media pipe carries the probe streams 2 and
//! 3 at the signal's level and the listen streams 0 and 1 only silence (the
//! owner's rule, #9 2026-09-28).

use super::*;

/// The probe frames each probe stream must deliver.
const FRAMES: u32 = 20;

#[test]
fn a_listen_probe_goes_out_on_the_probe_streams_and_the_taps_stay_silent() {
    let e = Engine::start(
        Flags {
            test_signal: true,
            fault_injection: false,
        },
        InputSignal::Silence,
    );
    let mut ctl = e.client();
    ctl.hello(Role::Control);
    // mic1 open in both listened mixes: without the HIL gate their listen
    // streams would carry the sine.
    for (id, m) in [(1, "engineer"), (2, "member1")] {
        let open = Cmd::SetLevel {
            mix: MixId::new(m),
            source: Source::Input(InputId::new("mic1")),
            gain_db: Some(0.0),
            pan: None,
            muted: None,
        };
        assert!(ctl.request(id, open).error.is_none(), "{m}");
        let listen = Cmd::StartListen { mix: MixId::new(m) };
        assert!(ctl.request(10 + id, listen).error.is_none(), "{m}");
    }
    let media = media_client(&e.pipe);
    let mut sup = e.client();
    sup.hello(Role::Supervisor);
    // The probe follows the output fade like the spare outputs: the engine's
    // 500 ms start fade-in has ended by its first Status, which comes a
    // second after the control loop began (after the stream started).
    sup.wait(|m| matches!(m, EngineMsg::Status(_)).then_some(()));
    let probe = Cmd::HilTestSignal {
        input: InputId::new("mic1"),
        hz: 1000.0,
        dbfs: -20.0,
        ttl_s: 3.0,
        card_tx: vec![94, 95],
        listen: true,
    };
    let r = sup.request(20, probe);
    assert!(r.error.is_none(), "{:?}", r.error);
    let mut seen = [0u32; 4];
    let mut peak = [0.0f32; 2];
    let start = Instant::now();
    while seen.iter().any(|n| *n < FRAMES) && start.elapsed() < WAIT {
        let (h, samples) = media.try_recv().unwrap();
        assert_eq!((h.channels, usize::from(h.frames)), (2, FRAME_48K));
        let s = usize::from(h.stream);
        assert!(s < 4, "stream {s}");
        seen[s] += 1;
        if s < 2 {
            assert!(
                samples.iter().all(|x| *x == 0.0),
                "listen stream {s} carried the sine"
            );
        } else {
            let p = samples.iter().fold(0.0f32, |m, x| m.max(x.abs()));
            peak[s - 2] = peak[s - 2].max(p);
        }
    }
    assert!(seen.iter().all(|n| *n >= FRAMES), "{seen:?}");
    for (k, p) in peak.iter().enumerate() {
        let dbfs = 20.0 * f64::from(*p).log10();
        assert!((dbfs + 20.0).abs() <= 0.1, "stream {}: {dbfs} dBFS", k + 2);
    }
    drop(media);
    e.shutdown();
}
