//! The soak client's wire (S7 design note §4).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_limits() {
        let l = Limits::default();
        let secs = Duration::from_secs;
        assert_eq!((l.give_up, l.write_every), (secs(120), secs(60)));
        assert_eq!(l.read_timeout, Duration::from_millis(500));
    }

    #[test]
    fn a_socket_reopens_after_its_backoff_and_gives_up_once_down_for_the_bound() {
        let t0 = Instant::now();
        let bound = Duration::from_secs(1);
        let at = |ms| t0 + Duration::from_millis(ms);
        let secs = Duration::from_secs;
        // Never opened: down since the start.
        assert!(!Reopen::new(t0).failed(at(1_000), bound));
        let mut r = Reopen::new(t0);
        assert_eq!(r.wait(), secs(1));
        assert!(r.failed(at(999), bound), "down 999 ms: open again");
        assert_eq!(r.wait(), secs(2));
        assert!(r.failed(at(999), bound));
        assert_eq!(r.wait(), secs(4));
        // An open resets the backoff; the down clock starts at the close.
        r.opened();
        assert_eq!(r.wait(), secs(1));
        r.closed(at(5_000));
        assert!(r.failed(at(5_999), bound));
        assert!(
            !r.failed(at(6_000), bound),
            "down exactly the bound: give up"
        );
    }

    #[test]
    fn only_a_read_timeout_is_a_wait() {
        // WouldBlock on Unix, TimedOut on Windows.
        assert!(waited(&io::ErrorKind::WouldBlock.into()));
        assert!(waited(&io::ErrorKind::TimedOut.into()));
        for closed in [
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::UnexpectedEof,
            io::ErrorKind::BrokenPipe,
        ] {
            assert!(!waited(&closed.into()), "{closed:?}");
        }
    }

    #[test]
    fn the_listen_stop_is_the_servers_own() {
        let stop = serde_json::to_string(&iem_core::ClientMsg::ListenStop).unwrap();
        assert_eq!(LISTEN_STOP, stop);
    }
}
