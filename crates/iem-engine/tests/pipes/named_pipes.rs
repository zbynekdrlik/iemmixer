use super::*;
use iem_engine::pipe::{listen, sddl_is_private};
use iem_win::token::{current_user_sid, pipe_sddl, sddl_sid};

#[test]
fn a_client_that_stops_reading_is_dropped_and_the_engine_keeps_serving() {
    let e = Engine::start(
        Flags::default(),
        InputSignal::Sine {
            hz: 1000.0,
            amp: 0.1,
        },
    );
    let mut ctl = e.client();
    ctl.hello(Role::Control);
    let engineer = Cmd::StartListen {
        mix: MixId::new("engineer"),
    };
    assert!(ctl.request(1, engineer).error.is_none());
    // Two clients that never read: the engine's writes to them (the
    // topology and meters; listen frames) outgrow their pipes' 512-byte
    // buffers and wait for them.
    let pipe = e.pipe.clone();
    let stalled = connect(move || control_name(&pipe));
    let hello = ClientMsg::Hello {
        proto: PROTO,
        role: Role::Observe,
        client: "stalled".into(),
    };
    write_frame(&mut &stalled, &hello).unwrap();
    let pipe = e.pipe.clone();
    let stalled_media = connect(move || media_name(&pipe));
    let start = Instant::now();
    // The control thread gives the stalled client SEND_TIMEOUT (1 s) and
    // then drops it: the controller's replies keep coming.
    for id in 2..=6 {
        let asked = Instant::now();
        assert!(ctl.request(id, Cmd::Ping).error.is_none());
        let took = asked.elapsed();
        assert!(took < Duration::from_secs(2), "reply {id} after {took:?}");
    }
    // The media pump dropped its stalled client too: a new one is served.
    let media = media_client(&e.pipe);
    let (h, samples) = media.try_recv().unwrap();
    assert_eq!((h.channels, samples.len()), (2, 2 * FRAME_48K));
    // The stalled control client is gone: reading it now shows at most
    // what fitted its pipe before the drop, then the end.
    std::thread::sleep(Duration::from_secs(2).saturating_sub(start.elapsed()));
    let mut dropped = Client {
        r: Reader::start(stalled, Framer::next_frame),
    };
    assert!(dropped.closed(), "the stalled client was dropped");
    drop(stalled_media);
    drop(media);
    // `run` joins the media pump: the shutdown still ends it within 5 s.
    e.shutdown();
}

/// The engine closes a connection at once, even while its peer has not
/// read what the engine wrote last, so a peer that never reads holds up
/// no other close. interprocess's flush on drop (limbo) would keep the
/// engine's end open on the process's one linger thread until the peer
/// has read everything, and every stream dropped later in the process
/// would wait behind it, unclosed (Windows CI run 36373563262: two
/// clients never read the end the engine gave them, and the engine
/// never saw a controller leave). The peer still reads what came before
/// the close, then the end.
#[test]
fn a_peer_that_does_not_read_holds_up_no_close() {
    use iem_win::pipe::{available, write_within};
    use std::os::windows::io::AsHandle;

    let e = Engine::start(Flags::default(), InputSignal::Silence);
    let pipe = e.pipe.clone();
    let mute = connect(move || control_name(&pipe));
    // Refused at its hello: a short reply that fits the pipe, then the
    // engine drops the connection. Nobody reads that reply yet.
    let hello = ClientMsg::Hello {
        proto: 0,
        role: Role::Observe,
        client: "mute".into(),
    };
    write_frame(&mut &mute, &hello).unwrap();
    let gone = {
        let Stream::NamedPipe(end) = &mute;
        let end = end.inner().as_handle();
        let start = Instant::now();
        while available(end).unwrap() == 0 {
            assert!(start.elapsed() < WAIT, "no reply to the refused hello");
            std::thread::sleep(Duration::from_millis(10));
        }
        // Once the engine's end is closed, a byte the client writes
        // finds the pipe closing; while it stays open, each byte waits
        // in the pipe (a few dozen fit its 512 bytes).
        let start = Instant::now();
        loop {
            match write_within(end, &[0], Duration::from_millis(100)) {
                Err(gone) => break Some(gone),
                Ok(_) if start.elapsed() < Duration::from_secs(2) => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(_) => break None,
            }
        }
    };
    // Meanwhile a controller superseded by a new one reads the end of
    // its stream.
    let mut first = e.client();
    first.hello(Role::Control);
    let mut second = e.client();
    second.hello(Role::Control);
    first.wait(|m| matches!(m, EngineMsg::Superseded).then_some(()));
    let later_closed = first.closed();
    // The refused client reads the reply the engine wrote before it
    // closed, then the end (reading it also frees whatever waited for
    // it, before any assertion below can fail).
    let mut late = Client {
        r: Reader::start(mute, Framer::next_frame),
    };
    let reply = late.wait(|m| match m {
        EngineMsg::Reply(r) => Some(r.clone()),
        _ => None,
    });
    assert_eq!(reply.error.map(|b| b.code), Some(ErrCode::Unsupported));
    assert!(late.closed(), "the refused client reads the end");
    let Some(gone) = gone else {
        panic!("the engine kept a closed connection open until its peer read it");
    };
    assert!(
        matches!(gone.raw_os_error(), Some(109 | 232 | 233)),
        "{gone}"
    );
    assert!(later_closed, "a close waited for a peer that does not read");
    e.shutdown();
}

/// `ERROR_PIPE_BUSY`: every instance is taken for a moment.
const BUSY: i32 = 231;

/// The pipe's DACL; waits while the engine has not yet made a new
/// instance after the last connection.
fn read_dacl(name: &str) -> String {
    let start = Instant::now();
    loop {
        match pipe_sddl(name) {
            Ok(text) => return text,
            Err(e) if e.raw_os_error() == Some(BUSY) && start.elapsed() < WAIT => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("{name}: {e}"),
        }
    }
}

#[test]
fn a_second_listener_on_a_held_name_is_refused_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let name = pipe_name(&dir);
    let first = listen(control_name(&name).unwrap()).unwrap();
    // A live listener holds the name as long as it lives (a gone one's
    // client does not, below), so waiting cannot free it: a second
    // engine or a squatter is refused at once.
    let start = Instant::now();
    let second = listen(control_name(&name).unwrap()).unwrap_err();
    let took = start.elapsed();
    assert_eq!(second.kind(), std::io::ErrorKind::AddrInUse, "{second}");
    assert!(
        second.to_string().starts_with("pipe name taken ("),
        "{second}"
    );
    assert!(took < Duration::from_secs(1), "{took:?}");
    // So it stays while the listener serves: a connection accepted and
    // dropped, its next instance listening.
    let n = name.clone();
    let client = connect(move || control_name(&n));
    drop(accept(&first));
    let third = listen(control_name(&name).unwrap()).unwrap_err();
    assert_eq!(third.kind(), std::io::ErrorKind::AddrInUse, "{third}");
    drop(client);
}

fn accept(listener: &interprocess::local_socket::Listener) -> Stream {
    use interprocess::local_socket::traits::Listener as _;
    let start = Instant::now();
    loop {
        match listener.accept() {
            Ok(s) => return s,
            Err(e) => {
                assert!(start.elapsed() < WAIT, "accept: {e}");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

#[test]
fn a_gone_listeners_name_is_free_while_its_client_still_holds_its_end() {
    let dir = tempfile::tempdir().unwrap();
    let name = pipe_name(&dir);
    let first = listen(control_name(&name).unwrap()).unwrap();
    let n = name.clone();
    let client = connect(move || control_name(&n));
    drop(accept(&first));
    drop(first);
    // Every server end of the name is closed, as when an engine's
    // process has ended; its client still holds its end. A client's end
    // does not hold the name: a new listener (the respawned engine)
    // creates the first instance while the client is still there
    // (Windows CI run 36371298924 refuted the opposite).
    let again = listen(control_name(&name).unwrap())
        .unwrap_or_else(|e| panic!("the gone listener's client held the name: {e}"));
    // That client is no client of the new listener: its end reads the
    // end of the stream.
    let mut old = Client {
        r: Reader::start(client, Framer::next_frame),
    };
    assert!(old.closed(), "the gone listener's client reads the end");
    // A new client is the new listener's, which holds the name against
    // another listener.
    let n = name.clone();
    let fresh = connect(move || control_name(&n));
    drop(accept(&again));
    let squatter = listen(control_name(&name).unwrap()).unwrap_err();
    assert_eq!(squatter.kind(), std::io::ErrorKind::AddrInUse, "{squatter}");
    assert!(
        squatter.to_string().starts_with("pipe name taken ("),
        "{squatter}"
    );
    drop(fresh);
}

#[test]
fn a_second_engine_on_a_held_pipe_stops_with_an_io_error() {
    let e = Engine::start(Flags::default(), InputSignal::Silence);
    e.client().hello(Role::Observe);
    let dir = tempfile::tempdir().unwrap();
    let cfg = RunConfig::new(
        common::site_path(),
        dir.path().join("state"),
        e.pipe.clone(),
    );
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(run(cfg));
    });
    let err = rx
        .recv_timeout(WAIT)
        .expect("a refused engine returns at once")
        .unwrap_err();
    assert!(
        matches!(&err, EngineError::Io(io) if io.kind() == std::io::ErrorKind::AddrInUse),
        "{err}"
    );
    assert!(err.to_string().starts_with("pipe name taken ("), "{err}");
    e.shutdown();
}

#[test]
fn the_engines_pipes_admit_only_the_user_and_system() {
    let e = Engine::start(Flags::default(), InputSignal::Silence);
    e.client().hello(Role::Observe);
    let user = current_user_sid().unwrap();
    let written = sddl_sid(&user).unwrap();
    // No media client is connected: reading the media pipe's DACL
    // connects as one for a moment (`pipe_sddl`), superseding nobody.
    for name in [e.pipe.clone(), format!("{}.media", e.pipe)] {
        let dacl = read_dacl(&name);
        assert!(
            sddl_is_private(&dacl, &written),
            "{name}: {dacl} (user {user}, written {written})"
        );
        assert!(dacl.contains(";;;SY)"), "{name}: {dacl}");
        // Protected: nothing is inherited into it.
        assert!(dacl.starts_with("D:P("), "{name}: {dacl}");
    }
    e.shutdown();
}
