//! The guard's alarms (S6 design note §5.4).
//!
//! A persistent list, shown by `iemmode` and the tray on every call and sent
//! to the engineer's devices: the mixer app's (the PWA's) notification
//! subscriptions, where the predecessor's alerts went (#9 2026-09-28). The
//! newest [`Alarms::KEEP`] are kept; ids are never reused.

use serde::{Deserialize, Serialize};

use crate::plan::Step;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alarm {
    pub id: u64,
    /// Seconds since the epoch.
    pub at: u64,
    /// The switch step that failed, if a step did.
    #[serde(default)]
    pub step: Option<Step>,
    pub text: String,
    #[serde(default)]
    pub acked: bool,
    /// Sent to the engineer's devices (`iem-server notify --to alarm`).
    #[serde(default)]
    pub notified: bool,
    /// The agent must send the owner the prepared question (a plan stopped).
    #[serde(default)]
    pub owner_question: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Alarms {
    /// The id of the newest alarm ever raised (0: none yet).
    last_id: u64,
    list: Vec<Alarm>,
}

impl Alarms {
    /// How many alarms are kept (the newest).
    pub const KEEP: usize = 50;

    /// Raises an alarm; returns its id.
    pub fn raise(
        &mut self,
        at: u64,
        step: Option<Step>,
        text: impl Into<String>,
        owner_question: bool,
    ) -> u64 {
        self.last_id += 1;
        let id = self.last_id;
        self.list.push(Alarm {
            id,
            at,
            step,
            text: text.into(),
            acked: false,
            notified: false,
            owner_question,
        });
        let over = self.list.len().saturating_sub(Self::KEEP);
        self.list.drain(..over);
        id
    }

    fn find(&mut self, id: u64) -> Option<&mut Alarm> {
        self.list.iter_mut().find(|a| a.id == id)
    }

    /// Acknowledges an alarm; false when no alarm with that id is kept.
    pub fn ack(&mut self, id: u64) -> bool {
        match self.find(id) {
            Some(a) => {
                a.acked = true;
                true
            }
            None => false,
        }
    }

    /// Records that the alarm reached the engineer's devices; false when no
    /// alarm with that id is kept.
    pub fn mark_notified(&mut self, id: u64) -> bool {
        match self.find(id) {
            Some(a) => {
                a.notified = true;
                true
            }
            None => false,
        }
    }

    /// The kept alarms, oldest first.
    pub fn all(&self) -> &[Alarm] {
        &self.list
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Alarm> {
        self.list.iter()
    }

    pub fn last(&self) -> Option<&Alarm> {
        self.list.last()
    }

    /// How many kept alarms nobody acknowledged (the tray's count).
    pub fn unacked(&self) -> usize {
        self.list.iter().filter(|a| !a.acked).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alarms_get_increasing_ids_and_keep_their_fields() {
        let mut a = Alarms::default();
        assert_eq!(a.last(), None);
        assert_eq!(a.raise(10, None, "drift", false), 1);
        assert_eq!(
            a.raise(
                11,
                Some(Step::PrefCheck),
                String::from("3 restores failed"),
                true
            ),
            2
        );
        assert_eq!(
            a.all(),
            [
                Alarm {
                    id: 1,
                    at: 10,
                    step: None,
                    text: "drift".into(),
                    acked: false,
                    notified: false,
                    owner_question: false,
                },
                Alarm {
                    id: 2,
                    at: 11,
                    step: Some(Step::PrefCheck),
                    text: "3 restores failed".into(),
                    acked: false,
                    notified: false,
                    owner_question: true,
                },
            ]
        );
        assert_eq!(a.last().map(|x| x.id), Some(2));
        assert_eq!(a.iter().map(|x| x.at).collect::<Vec<_>>(), [10, 11]);
        assert_eq!(a.unacked(), 2);
    }

    #[test]
    fn only_the_newest_fifty_are_kept_and_ids_are_never_reused() {
        let mut a = Alarms::default();
        for i in 0..Alarms::KEEP as u64 {
            a.raise(i, None, "x", false);
        }
        assert_eq!(a.all().len(), 50);
        assert_eq!(a.all()[0].id, 1);
        assert_eq!(a.raise(99, None, "y", false), 51);
        assert_eq!(a.all().len(), 50);
        assert_eq!(a.all()[0].id, 2);
        for _ in 0..10 {
            a.raise(100, None, "z", false);
        }
        assert_eq!(a.all().len(), 50);
        assert_eq!(a.all()[0].id, 12);
        assert_eq!(a.last().map(|x| x.id), Some(61));
    }

    #[test]
    fn ack_and_notified_touch_only_their_alarm() {
        let mut a = Alarms::default();
        let one = a.raise(1, None, "one", false);
        let two = a.raise(2, None, "two", true);
        let three = a.raise(3, None, "three", false);
        assert!(a.ack(two));
        assert_eq!(a.unacked(), 2);
        assert_eq!(
            a.all().iter().map(|x| x.acked).collect::<Vec<_>>(),
            [false, true, false]
        );
        assert!(a.mark_notified(three));
        assert_eq!(
            a.all().iter().map(|x| x.notified).collect::<Vec<_>>(),
            [false, false, true]
        );
        assert!(!a.ack(99));
        assert!(!a.mark_notified(99));
        assert!(a.ack(one) && a.ack(three));
        assert_eq!(a.unacked(), 0);
    }

    #[test]
    fn alarms_round_trip_as_json() {
        let mut a = Alarms::default();
        a.raise(
            5,
            Some(Step::EngineStop),
            "no DriverReleased within 10 s",
            true,
        );
        let text = serde_json::to_string(&a).unwrap();
        let mut back: Alarms = serde_json::from_str(&text).unwrap();
        assert_eq!(back, a);
        // The id counter survives a reload.
        assert_eq!(back.raise(6, None, "next", false), 2);
        assert_eq!(
            serde_json::from_str::<Alarms>("{}").unwrap(),
            Alarms::default()
        );
    }
}
