//! What the tray shows of the guard (S6 plan Task 11, F27): the tooltip for
//! the guard's state and the notification of new alarms, from the frames of
//! its subscription ([`crate::proto::Update`]). Pure, so it is tested and
//! mutated on Linux; the tray (Windows only) only moves the text to its icon.

use crate::alarms::Alarm;
use crate::plan::{Mode, Step};
use crate::proto::Reply;

/// The first word of every tooltip and notification title.
const NAME: &str = "iemmixer";

/// The mode as `iemmode` and the guard's state file name it.
pub fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Event => "event",
        Mode::Dev => "dev",
        Mode::Live => "live",
    }
}

/// A step as the guard's state file names it (`pref_check`).
pub fn step_name(step: Step) -> String {
    serde_json::to_value(step)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// A count of alarms in Slovak: "1 alarm", "2 alarmy" to "4 alarmy",
/// otherwise "N alarmov".
pub fn alarm_count(n: usize) -> String {
    let word = match n {
        1 => "alarm",
        2..=4 => "alarmy",
        _ => "alarmov",
    };
    format!("{n} {word}")
}

/// The tray's tooltip: the mode ("iemmixer — dev"), or the switch in progress
/// ("iemmixer — dev → event"), and the unacknowledged alarms
/// ("iemmixer — dev, 2 alarmy"). `None`: no guard answers.
pub fn tooltip(reply: Option<&Reply>) -> String {
    let Some(reply) = reply else {
        return format!("{NAME} — bez spojenia so strážcom");
    };
    let state = match &reply.switching {
        Some(s) => format!("{} → {}", mode_name(s.from), mode_name(s.to)),
        None => mode_name(reply.mode).to_owned(),
    };
    let open = reply.alarms.iter().filter(|a| !a.acked).count();
    if open == 0 {
        format!("{NAME} — {state}")
    } else {
        format!("{NAME} — {state}, {}", alarm_count(open))
    }
}

/// What a subscriber has seen of the guard's alarms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    /// No reply yet; the subscriber started at this time (seconds since the
    /// epoch).
    Start(u64),
    /// The newest alarm of the last reply as (id, raised at); none for an
    /// empty list.
    Newest(Option<(u64, u64)>),
}

/// The alarms a subscriber has seen (the tray's notifications).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    mark: Mark,
}

impl Seen {
    /// A subscriber that started at `started` (seconds since the epoch).
    pub fn new(started: u64) -> Self {
        Seen {
            mark: Mark::Start(started),
        }
    }

    /// The unacknowledged alarms of `reply` to announce, oldest first.
    ///
    /// The first reply announces those raised since the subscriber started,
    /// or since the start of the switch in progress when that is earlier:
    /// the guard starts the tray in the middle of a dev entry (design note
    /// §5.2 step 8), after the entry's own alarms. Older ones are only
    /// counted in the tooltip. Every later reply (after a reconnect too)
    /// announces those newer than the newest alarm seen. When that alarm is
    /// no longer in the list, the list started over (a guard that lost its
    /// state file) or ran past it: every alarm in it is new.
    pub fn fresh<'a>(&mut self, reply: &'a Reply) -> Vec<&'a Alarm> {
        let newest = reply
            .alarms
            .iter()
            .max_by_key(|a| a.id)
            .map(|a| (a.id, a.at));
        let before = std::mem::replace(&mut self.mark, Mark::Newest(newest));
        let open = reply.alarms.iter().filter(|a| !a.acked);
        match before {
            Mark::Start(started) => {
                let since = reply
                    .switching
                    .as_ref()
                    .map_or(started, |s| s.started.min(started));
                open.filter(|a| a.at >= since).collect()
            }
            Mark::Newest(Some((id, at)))
                if reply.alarms.iter().any(|a| a.id == id && a.at == at) =>
            {
                open.filter(|a| a.id > id).collect()
            }
            Mark::Newest(_) => open.collect(),
        }
    }
}

/// An alarm's text, after the failed step's name when a step failed.
fn describe(alarm: &Alarm) -> String {
    match alarm.step {
        Some(step) => format!("{}: {}", step_name(step), alarm.text),
        None => alarm.text.clone(),
    }
}

/// The notification for the alarms [`Seen::fresh`] returned, as (title,
/// text): none for none; one alarm with its own text; several as their
/// count with the newest one's text (one notification, not a burst).
pub fn notice(fresh: &[&Alarm]) -> Option<(String, String)> {
    let newest = fresh.last()?;
    let title = match fresh.len() {
        1 => format!("{NAME} — alarm"),
        n => format!("{NAME} — {}", alarm_count(n)),
    };
    Some((title, describe(newest)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alarms::Alarms;
    use crate::state::Switching;

    fn reply(mode: Mode, alarms: &Alarms) -> Reply {
        Reply {
            ok: true,
            mode,
            switching: None,
            alarms: alarms.all().to_vec(),
            detail: String::new(),
            engine: None,
            guard_build: None,
        }
    }

    fn ids(alarms: &[&Alarm]) -> Vec<u64> {
        alarms.iter().map(|a| a.id).collect()
    }

    #[test]
    fn modes_and_steps_have_the_guard_s_names() {
        assert_eq!(mode_name(Mode::Event), "event");
        assert_eq!(mode_name(Mode::Dev), "dev");
        assert_eq!(mode_name(Mode::Live), "live");
        for mode in [Mode::Event, Mode::Dev, Mode::Live] {
            assert_eq!(
                serde_json::to_string(&mode).unwrap(),
                format!("\"{}\"", mode_name(mode))
            );
        }
        assert_eq!(step_name(Step::PrefCheck), "pref_check");
        assert_eq!(step_name(Step::ReaperSaveQuit), "reaper_save_quit");
        for step in Step::ALL {
            assert_eq!(
                serde_json::to_string(&step).unwrap(),
                format!("\"{}\"", step_name(step))
            );
        }
    }

    #[test]
    fn alarm_counts_are_slovak() {
        assert_eq!(alarm_count(0), "0 alarmov");
        assert_eq!(alarm_count(1), "1 alarm");
        assert_eq!(alarm_count(2), "2 alarmy");
        assert_eq!(alarm_count(3), "3 alarmy");
        assert_eq!(alarm_count(4), "4 alarmy");
        assert_eq!(alarm_count(5), "5 alarmov");
        assert_eq!(alarm_count(21), "21 alarmov");
    }

    #[test]
    fn the_tooltip_shows_the_mode_the_switch_and_the_open_alarms() {
        assert_eq!(tooltip(None), "iemmixer — bez spojenia so strážcom");
        let mut alarms = Alarms::default();
        assert_eq!(tooltip(Some(&reply(Mode::Dev, &alarms))), "iemmixer — dev");
        assert_eq!(
            tooltip(Some(&reply(Mode::Event, &alarms))),
            "iemmixer — event"
        );
        alarms.raise(10, None, "tuning drift", false);
        assert_eq!(
            tooltip(Some(&reply(Mode::Live, &alarms))),
            "iemmixer — live, 1 alarm"
        );
        alarms.raise(11, Some(Step::PrefCheck), "3 restores failed", true);
        let mut switching = reply(Mode::Dev, &alarms);
        switching.switching = Some(Switching {
            from: Mode::Dev,
            to: Mode::Event,
            done: vec![Step::JobsCancel],
            started: 12,
        });
        assert_eq!(
            tooltip(Some(&switching)),
            "iemmixer — dev → event, 2 alarmy"
        );
        // Acknowledged alarms are not counted.
        alarms.ack(1);
        assert_eq!(
            tooltip(Some(&reply(Mode::Dev, &alarms))),
            "iemmixer — dev, 1 alarm"
        );
        alarms.ack(2);
        assert_eq!(tooltip(Some(&reply(Mode::Dev, &alarms))), "iemmixer — dev");
    }

    #[test]
    fn the_first_reply_announces_the_alarms_since_the_start() {
        let mut alarms = Alarms::default();
        alarms.raise(99, None, "before the tray", false);
        alarms.raise(100, None, "as the tray started", false);
        alarms.raise(101, None, "after the start", false);
        alarms.raise(102, None, "acknowledged", false);
        alarms.ack(4);
        let mut seen = Seen::new(100);
        assert_eq!(ids(&seen.fresh(&reply(Mode::Dev, &alarms))), vec![2, 3]);
        // The same list again: nothing new.
        assert!(seen.fresh(&reply(Mode::Dev, &alarms)).is_empty());
        alarms.raise(103, None, "new", false);
        let r = reply(Mode::Dev, &alarms);
        assert_eq!(ids(&seen.fresh(&r)), vec![5]);
        assert!(seen.fresh(&r).is_empty());
    }

    #[test]
    fn the_first_reply_in_a_switch_announces_the_switch_s_alarms() {
        let mut alarms = Alarms::default();
        alarms.raise(49, None, "before the switch", false);
        alarms.raise(50, Some(Step::Interlock), "refused once", false);
        alarms.raise(60, None, "tuning module missing", false);
        let mut r = reply(Mode::Dev, &alarms);
        r.switching = Some(Switching {
            from: Mode::Event,
            to: Mode::Dev,
            done: vec![Step::Precheck, Step::Interlock],
            started: 50,
        });
        // The guard started the tray in the switch, after its alarms.
        let mut seen = Seen::new(70);
        assert_eq!(ids(&seen.fresh(&r)), vec![2, 3]);
        assert!(seen.fresh(&r).is_empty());
        // A switch that started after the tray: the tray's start counts.
        if let Some(s) = r.switching.as_mut() {
            s.started = 65;
        }
        let mut seen = Seen::new(55);
        assert_eq!(ids(&seen.fresh(&r)), vec![3]);
        // No alarm since either start.
        let mut seen = Seen::new(61);
        assert!(seen.fresh(&r).is_empty());
    }

    #[test]
    fn a_first_reply_without_alarms_announces_the_first_alarm() {
        let mut alarms = Alarms::default();
        let mut seen = Seen::new(100);
        assert!(seen.fresh(&reply(Mode::Dev, &alarms)).is_empty());
        // After the first reply the time no longer matters, only the ids.
        alarms.raise(10, None, "first", false);
        assert_eq!(ids(&seen.fresh(&reply(Mode::Dev, &alarms))), vec![1]);
    }

    #[test]
    fn every_unacknowledged_newer_alarm_is_announced_once() {
        let mut alarms = Alarms::default();
        alarms.raise(10, None, "a", false);
        let mut seen = Seen::new(100);
        assert!(seen.fresh(&reply(Mode::Dev, &alarms)).is_empty());
        alarms.raise(11, None, "b", false);
        alarms.raise(12, None, "c", false);
        alarms.raise(13, None, "d", false);
        alarms.ack(3);
        // Alarm 1 (seen) is still open, alarm 3 was acknowledged at once.
        assert_eq!(ids(&seen.fresh(&reply(Mode::Dev, &alarms))), vec![2, 4]);
        assert!(seen.fresh(&reply(Mode::Dev, &alarms)).is_empty());
        alarms.raise(14, None, "e", false);
        assert_eq!(ids(&seen.fresh(&reply(Mode::Event, &alarms))), vec![5]);
    }

    #[test]
    fn a_guard_whose_alarm_list_started_over_has_only_new_alarms() {
        let mut before = Alarms::default();
        for at in 0..3 {
            before.raise(at, None, "before", false);
        }
        let mut seen = Seen::new(100);
        assert!(seen.fresh(&reply(Mode::Dev, &before)).is_empty());
        // A lost state file: ids start at 1 again.
        let mut after = Alarms::default();
        after.raise(20, None, "guard state unreadable", false);
        after.raise(21, None, "drift", false);
        after.ack(2);
        assert_eq!(ids(&seen.fresh(&reply(Mode::Event, &after))), vec![1]);
        assert!(seen.fresh(&reply(Mode::Event, &after)).is_empty());
        after.raise(22, None, "next", false);
        assert_eq!(ids(&seen.fresh(&reply(Mode::Event, &after))), vec![3]);
        // The new list reaches past the old newest id (3, raised at 2): its
        // alarm 3 is another alarm, so every alarm in it is new.
        let mut seen = Seen::new(100);
        assert!(seen.fresh(&reply(Mode::Dev, &before)).is_empty());
        let mut past = Alarms::default();
        for at in 200..204 {
            past.raise(at, None, "after the loss", false);
        }
        assert_eq!(
            ids(&seen.fresh(&reply(Mode::Event, &past))),
            vec![1, 2, 3, 4]
        );
        assert!(seen.fresh(&reply(Mode::Event, &past)).is_empty());
        // An empty list lowers the watermark to nothing.
        let mut seen = Seen::new(100);
        assert!(seen.fresh(&reply(Mode::Dev, &before)).is_empty());
        assert!(seen.fresh(&reply(Mode::Dev, &Alarms::default())).is_empty());
        let mut again = Alarms::default();
        again.raise(30, None, "one", false);
        assert_eq!(ids(&seen.fresh(&reply(Mode::Dev, &again))), vec![1]);
    }

    #[test]
    fn a_list_that_ran_past_the_newest_seen_alarm_is_all_new() {
        let mut alarms = Alarms::default();
        alarms.raise(10, None, "seen", false);
        let mut seen = Seen::new(100);
        assert!(seen.fresh(&reply(Mode::Dev, &alarms)).is_empty());
        for at in 11..11 + Alarms::KEEP as u64 {
            alarms.raise(at, None, "burst", false);
        }
        let fresh = ids(&seen.fresh(&reply(Mode::Dev, &alarms)));
        assert_eq!(fresh.len(), Alarms::KEEP);
        assert_eq!(fresh.first(), Some(&2));
        assert_eq!(fresh.last(), Some(&51));
    }

    fn pair(title: &str, text: &str) -> Option<(String, String)> {
        Some((title.to_owned(), text.to_owned()))
    }

    #[test]
    fn one_notice_per_update() {
        let mut alarms = Alarms::default();
        alarms.raise(10, None, "tuning drift", false);
        alarms.raise(11, Some(Step::PrefCheck), "3 restores failed", true);
        alarms.raise(12, None, "a REAPER process appeared in dev", false);
        let all: Vec<&Alarm> = alarms.iter().collect();
        assert_eq!(notice(&[]), None);
        assert_eq!(notice(&all[..1]), pair("iemmixer — alarm", "tuning drift"));
        assert_eq!(
            notice(&all[1..2]),
            pair("iemmixer — alarm", "pref_check: 3 restores failed")
        );
        assert_eq!(
            notice(&all[..2]),
            pair("iemmixer — 2 alarmy", "pref_check: 3 restores failed")
        );
        assert_eq!(
            notice(&all),
            pair("iemmixer — 3 alarmy", "a REAPER process appeared in dev")
        );
    }
}
