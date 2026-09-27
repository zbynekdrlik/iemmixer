//! The talkback lock (F18, X6, program spec §5.3): one talker at a time,
//! held by a mixer session. Acquiring hands out a 128-bit talk id; the
//! talkback socket binds by presenting it within 1 s; the lock is released
//! on stop, when the holder's mixer socket closes, or after 2 s without
//! talkback frames (which also ends a lock whose socket never came).

use std::time::{Duration, Instant};

pub const BIND_WINDOW: Duration = Duration::from_secs(1);
pub const SILENCE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
struct Held {
    session: u64,
    who: String,
    id: String,
    since: Instant,
    bound: bool,
    last_frame: Instant,
}

#[derive(Debug, Default)]
pub struct TalkLock {
    held: Option<Held>,
}

/// A fresh 128-bit talk id as 32 hex digits.
pub fn new_talk_id() -> String {
    use rand_core::RngCore;
    let mut bytes = [0u8; 16];
    rand_core::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl TalkLock {
    /// Takes the lock for `session` with `id`; a session that holds it keeps
    /// its id. `Err` names the holder.
    pub fn acquire(
        &mut self,
        session: u64,
        who: &str,
        id: String,
        now: Instant,
    ) -> Result<String, String> {
        match &self.held {
            Some(h) if h.session == session => Ok(h.id.clone()),
            Some(h) => Err(h.who.clone()),
            None => {
                self.held = Some(Held {
                    session,
                    who: who.to_string(),
                    id: id.clone(),
                    since: now,
                    bound: false,
                    last_frame: now,
                });
                Ok(id)
            }
        }
    }

    /// A talkback socket presents `id` at `now`: the holder's session when
    /// it may bind (right id, not bound yet, inside the window).
    pub fn bind(&mut self, id: &str, now: Instant) -> Option<u64> {
        let h = self.held.as_mut()?;
        if h.id != id || h.bound || now.saturating_duration_since(h.since) > BIND_WINDOW {
            return None;
        }
        h.bound = true;
        h.last_frame = now;
        Some(h.session)
    }

    /// A talkback frame arrived for the bound id.
    pub fn frame(&mut self, id: &str, now: Instant) {
        if let Some(h) = self.held.as_mut().filter(|h| h.id == id) {
            h.last_frame = now;
        }
    }

    /// Releases the lock if `session` holds it.
    pub fn release(&mut self, session: u64) -> bool {
        if self.held.as_ref().is_some_and(|h| h.session == session) {
            self.held = None;
            true
        } else {
            false
        }
    }

    /// Ends a lock without frames for 2 s; the session that held it.
    pub fn expire(&mut self, now: Instant) -> Option<u64> {
        let quiet = self
            .held
            .as_ref()
            .is_some_and(|h| now.saturating_duration_since(h.last_frame) > SILENCE);
        if quiet {
            self.held.take().map(|h| h.session)
        } else {
            None
        }
    }

    /// Whether `id` is the held, bound talk id.
    pub fn is_bound(&self, id: &str) -> bool {
        self.held.as_ref().is_some_and(|h| h.bound && h.id == id)
    }

    /// Who talks, if anyone.
    pub fn holder(&self) -> Option<&str> {
        self.held.as_ref().map(|h| h.who.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn talk_ids_are_128_bit_hex_and_fresh() {
        let a = new_talk_id();
        assert_eq!(a.len(), 32);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_ne!(a, new_talk_id());
    }

    #[test]
    fn one_talker_at_a_time() {
        let t = Instant::now();
        let mut l = TalkLock::default();
        assert_eq!(l.acquire(1, "engineer", "aa".into(), t), Ok("aa".into()));
        assert_eq!(l.acquire(1, "engineer", "bb".into(), t), Ok("aa".into()));
        assert_eq!(
            l.acquire(2, "engineer", "cc".into(), t),
            Err("engineer".into())
        );
        assert_eq!(l.holder(), Some("engineer"));
        assert!(!l.release(2));
        assert!(l.release(1));
        assert_eq!(l.holder(), None);
        assert!(!l.release(1));
        assert_eq!(l.acquire(2, "engineer", "cc".into(), t), Ok("cc".into()));
    }

    #[test]
    fn the_socket_binds_once_with_the_held_id_inside_one_second() {
        let t = Instant::now();
        let mut l = TalkLock::default();
        assert_eq!(l.bind("aa", t), None, "nothing held");
        l.acquire(7, "engineer", "aa".into(), t).unwrap();
        assert_eq!(l.bind("zz", t), None);
        assert!(!l.is_bound("aa"));
        assert_eq!(l.bind("aa", t + BIND_WINDOW), Some(7));
        assert!(l.is_bound("aa"));
        assert!(!l.is_bound("zz"));
        assert_eq!(l.bind("aa", t), None, "already bound");
        l.release(7);
        l.acquire(8, "engineer", "bb".into(), t).unwrap();
        assert_eq!(
            l.bind("bb", t + BIND_WINDOW + Duration::from_millis(1)),
            None,
            "too late"
        );
    }

    #[test]
    fn two_seconds_without_frames_release_the_lock() {
        let t = Instant::now();
        let mut l = TalkLock::default();
        assert_eq!(l.expire(t + Duration::from_secs(9)), None);
        l.acquire(3, "engineer", "aa".into(), t).unwrap();
        assert_eq!(
            l.expire(t + SILENCE),
            None,
            "exactly 2 s is not yet silence"
        );
        l.bind("aa", t + Duration::from_millis(500));
        l.frame("aa", t + Duration::from_secs(2));
        l.frame("zz", t + Duration::from_secs(9));
        assert_eq!(l.expire(t + Duration::from_millis(4_000)), None);
        assert_eq!(l.expire(t + Duration::from_millis(4_001)), Some(3));
        assert_eq!(l.holder(), None);
        // A socket that never came: released 2 s after acquiring.
        l.acquire(4, "engineer", "cc".into(), t).unwrap();
        assert_eq!(l.expire(t + Duration::from_millis(2_001)), Some(4));
    }
}
