// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! The latency guard in health (#122). Every value is synthetic (P6):
//! channel numbers within the test site's, the box's shape of
//! `scripts/iem-pc/test_gen3_latency.py`: D 56, a stage rack's flow at
//! delay 16 feeding RX 1 and 3, a wireless device's at 48 feeding RX 2, the
//! members' in-ear TX flow at 250 µs (24 samples) and the engineer's at
//! 1 ms, which carries no in-ear output.

use super::*;

const RATE: u64 = 96_000;

fn guard(inputs: &[(&[u64], u64)]) -> Guard {
    Guard {
        rate: RATE,
        in_ear_tx: vec![1, 2, 3, 4, 23],
        inputs: inputs
            .iter()
            .map(|(rx, budget)| Input {
                rx: rx.to_vec(),
                budget: *budget,
            })
            .collect(),
    }
}

/// mic1 on RX 1, mic2 (the wireless one, its own budget 128) on RX 2,
/// mic3 on RX 3.
fn box_guard() -> Guard {
    guard(&[(&[1], 96), (&[2], 128), (&[3], 96)])
}

fn rx(channels: &[u64], delay: u64) -> RxFlow {
    RxFlow {
        rx_channels: Some(channels.to_vec()),
        delay: Some(delay),
    }
}

fn tx(channels: &[Option<u64>], us: u64) -> TxFlow {
    TxFlow {
        channels: channels.len() as u64,
        tx_channels: Some(channels.to_vec()),
        receiver_latency_us: us,
    }
}

fn box_flows() -> Flows {
    Flows {
        engine: Some(Engine { offset: Some(56) }),
        rx: vec![
            // An ended flow: no channels now, its old delay.
            RxFlow {
                rx_channels: None,
                delay: Some(900),
            },
            rx(&[1, 3], 16),
            rx(&[2], 48),
        ],
        tx: vec![
            tx(&[Some(1), Some(2), Some(3), None], 250),
            // An ended slot.
            TxFlow {
                channels: 0,
                tx_channels: None,
                receiver_latency_us: 0,
            },
            tx(&[Some(21), Some(22), None, None], 1000),
        ],
    }
}

fn line(inputs: u64, known: u64, over: u64, unread_tx: u64, worst_samples: Option<u64>) -> Line {
    Line {
        inputs,
        known,
        over,
        unread_tx,
        worst_samples,
        budget_samples: Some(96),
    }
}

#[test]
fn the_box_today_every_path_at_its_budget_is_no_reason() {
    // 56 + 16 + 24 = 96 at 1 ms; the wireless 56 + 48 + 24 = 128 at its own 128.
    let j = judge(&box_guard(), Some(&box_flows()));
    assert_eq!(j.line, line(3, 3, 0, 0, Some(128)));
    assert_eq!(j.reasons, []);
}

#[test]
fn a_known_path_one_sample_above_its_budget_is_latency_over_budget() {
    for (budget, over) in [(128, 0), (127, 1)] {
        let j = judge(&guard(&[(&[2], budget)]), Some(&box_flows()));
        assert_eq!(j.line, line(1, 1, over, 0, Some(128)), "budget {budget}");
        let want: &[Reason] = if over == 1 {
            &[Reason::LatencyOverBudget]
        } else {
            &[]
        };
        assert_eq!(j.reasons, want, "budget {budget}");
    }
    // Two inputs over count two, the reason once.
    let j = judge(
        &guard(&[(&[1], 95), (&[2], 96), (&[3], 96)]),
        Some(&box_flows()),
    );
    assert_eq!(j.line, line(3, 3, 2, 0, Some(128)));
    assert_eq!(j.reasons, [Reason::LatencyOverBudget]);
}

#[test]
fn each_part_of_the_path_counts() {
    // D, the RX flow's delay and the in-ear latency each move the path.
    let mut flows = box_flows();
    flows.engine = Some(Engine { offset: Some(57) });
    assert_eq!(
        judge(&box_guard(), Some(&flows)).line.worst_samples,
        Some(129)
    );
    let mut flows = box_flows();
    flows.rx[2].delay = Some(50);
    assert_eq!(
        judge(&box_guard(), Some(&flows)).line.worst_samples,
        Some(130)
    );
    let mut flows = box_flows();
    flows.tx[0].receiver_latency_us = 500; // 48 samples
    assert_eq!(
        judge(&box_guard(), Some(&flows)).line.worst_samples,
        Some(152)
    );
}

#[test]
fn the_in_ear_latency_rounds_up_to_whole_samples() {
    assert_eq!(samples_of_us(250, 96_000), 24);
    assert_eq!(samples_of_us(251, 96_000), 25);
    assert_eq!(samples_of_us(250, 44_100), 12);
    assert_eq!(samples_of_us(0, 96_000), 0);
    assert_eq!(samples_of_us(u64::MAX, 96_000), u64::MAX / 1_000_000 + 1);
}

#[test]
fn the_engineer_s_flow_carries_no_in_ear_output_and_counts_nothing() {
    // Its 1 ms would make every path 152; without the members' flow no
    // in-ear latency is known at all.
    let mut flows = box_flows();
    flows.tx.remove(0);
    let j = judge(&box_guard(), Some(&flows));
    assert_eq!(j.line, line(3, 0, 0, 0, None));
    assert_eq!(j.reasons, []);
    // And a 0 on it is no unread in-ear flow.
    flows.tx[1].receiver_latency_us = 0;
    assert_eq!(judge(&box_guard(), Some(&flows)).line.unread_tx, 0);
}

#[test]
fn the_worst_in_ear_flow_counts() {
    let mut flows = box_flows();
    flows.tx.push(tx(&[Some(23)], 260)); // the translator's mix: 25 samples
    let j = judge(&box_guard(), Some(&flows));
    assert_eq!(j.line.worst_samples, Some(129));
    // An in-ear output in any slot of the flow counts, an empty slot none.
    let mut flows = box_flows();
    flows.tx[0] = tx(&[None, None, None, Some(4)], 250);
    assert_eq!(judge(&box_guard(), Some(&flows)).line.known, 3);
}

#[test]
fn an_in_ear_flow_whose_receiver_latency_is_not_known_is_latency_unread() {
    let mut flows = box_flows();
    flows.tx.push(tx(&[Some(3), Some(4)], 0));
    let j = judge(&box_guard(), Some(&flows));
    // The known flow still makes the paths known.
    assert_eq!(j.line, line(3, 3, 0, 1, Some(128)));
    assert_eq!(j.reasons, [Reason::LatencyUnread]);
    // Its only in-ear flow unread: no path known, the reason all the same.
    let mut flows = box_flows();
    flows.tx[0].receiver_latency_us = 0;
    let j = judge(&box_guard(), Some(&flows));
    assert_eq!(j.line, line(3, 0, 0, 1, None));
    assert_eq!(j.reasons, [Reason::LatencyUnread]);
    // Over and unread: both, in that order.
    let j = judge(
        &guard(&[(&[1], 10)]),
        Some(&{
            let mut f = box_flows();
            f.tx.push(tx(&[Some(1)], 0));
            f
        }),
    );
    assert_eq!(
        j.reasons,
        [Reason::LatencyOverBudget, Reason::LatencyUnread]
    );
}

#[test]
fn a_slot_without_a_flow_now_is_no_in_ear_flow() {
    // A slot count of 0 with channels left over reads as no flow.
    let mut flows = box_flows();
    flows.tx.push(TxFlow {
        channels: 0,
        tx_channels: Some(vec![Some(1)]),
        receiver_latency_us: 0,
    });
    assert_eq!(judge(&box_guard(), Some(&flows)).line.unread_tx, 0);
}

#[test]
fn a_channel_no_flow_carries_is_never_a_reason() {
    // A stage device switched off: its input is not known, nothing more.
    let j = judge(&guard(&[(&[1], 96), (&[9], 96)]), Some(&box_flows()));
    assert_eq!(j.line, line(2, 1, 0, 0, Some(96)));
    assert_eq!(j.reasons, []);
    // An input on two channels takes its worst carried one, the other skipped.
    let j = judge(&guard(&[(&[1, 2, 9], 96)]), Some(&box_flows()));
    assert_eq!(j.line, line(1, 1, 1, 0, Some(128)));
    let j = judge(&guard(&[(&[9, 1], 96)]), Some(&box_flows()));
    assert_eq!(j.line, line(1, 1, 0, 0, Some(96)));
}

#[test]
fn two_flows_feeding_one_channel_take_the_worst() {
    let mut flows = box_flows();
    flows.rx.push(rx(&[3], 32));
    let j = judge(&guard(&[(&[3], 200)]), Some(&flows));
    assert_eq!(j.line.worst_samples, Some(56 + 32 + 24));
    // A flow without a delay adds nothing.
    let mut flows = box_flows();
    flows.rx.push(RxFlow {
        rx_channels: Some(vec![3]),
        delay: None,
    });
    let j = judge(&guard(&[(&[3], 200)]), Some(&flows));
    assert_eq!(j.line.worst_samples, Some(96));
}

#[test]
fn without_the_engine_s_offset_no_path_is_known() {
    for engine in [None, Some(Engine { offset: None })] {
        let mut flows = box_flows();
        flows.engine = engine.clone();
        let j = judge(&box_guard(), Some(&flows));
        assert_eq!(j.line, line(3, 0, 0, 0, None), "{engine:?}");
        assert_eq!(j.reasons, []);
    }
}

#[test]
fn flows_that_do_not_read_are_latency_unread() {
    let j = judge(&box_guard(), None);
    assert_eq!(j.line, line(3, 0, 0, 0, None));
    assert_eq!(j.reasons, [Reason::LatencyUnread]);
}

#[test]
fn the_guard_s_budget_is_1_ms_at_its_rate_rounded_down() {
    for (rate, budget) in [(96_000, 96), (48_000, 48), (44_100, 44)] {
        let g = Guard {
            rate,
            ..box_guard()
        };
        assert_eq!(judge(&g, None).line.budget_samples, Some(budget));
    }
}

#[test]
fn the_guard_s_file_reads_its_numbers_only() {
    let text = r#"{"rate":96000,"in_ear_tx":[1,2,3,4,23],"inputs":[{"rx":[1],"budget":96},{"rx":[2],"budget":128},{"rx":[3],"budget":96}]}"#;
    assert_eq!(parse_guard(text), Some(box_guard()));
    assert_eq!(
        parse_guard(r#"{"rate":48000,"in_ear_tx":[],"inputs":[]}"#),
        Some(Guard {
            rate: 48_000,
            in_ear_tx: vec![],
            inputs: vec![]
        })
    );
    for bad in [
        "",
        "{}",
        r#"{"rate":0,"in_ear_tx":[1],"inputs":[]}"#,
        r#"{"rate":96000,"in_ear_tx":[0],"inputs":[]}"#,
        r#"{"rate":96000,"in_ear_tx":[1],"inputs":[{"rx":[],"budget":96}]}"#,
        r#"{"rate":96000,"in_ear_tx":[1],"inputs":[{"rx":[0],"budget":96}]}"#,
        r#"{"rate":96000,"in_ear_tx":[1],"inputs":[{"rx":[1]}]}"#,
        r#"{"rate":96000,"in_ear_tx":[1],"inputs":[{"rx":[1],"budget":96,"id":"x"}]}"#,
        r#"{"rate":96000,"in_ear_tx":[1],"inputs":[],"names":[]}"#,
        r#"{"rate":-1,"in_ear_tx":[1],"inputs":[]}"#,
    ] {
        assert_eq!(parse_guard(bad), None, "{bad}");
    }
    // Only the bad channel among good ones refuses.
    assert_eq!(
        parse_guard(r#"{"rate":96000,"in_ear_tx":[1,0],"inputs":[]}"#),
        None
    );
    assert_eq!(
        parse_guard(r#"{"rate":96000,"in_ear_tx":[1],"inputs":[{"rx":[1,0],"budget":1}]}"#),
        None
    );
}

#[test]
fn the_guard_s_file_absent_unreadable_or_read() {
    let dir = std::env::temp_dir().join(format!("iem-health-guard-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("latency-guard.json");
    let _ = std::fs::remove_file(&path);
    assert_eq!(read_guard(&path), GuardRead::Absent);
    std::fs::write(&path, "not json").unwrap();
    assert_eq!(read_guard(&path), GuardRead::Unreadable);
    std::fs::write(&path, r#"{"rate":96000,"in_ear_tx":[1],"inputs":[]}"#).unwrap();
    assert_eq!(
        read_guard(&path),
        GuardRead::Read(Guard {
            rate: 96_000,
            in_ear_tx: vec![1],
            inputs: vec![]
        })
    );
    // A folder at the path is not absent: it does not read.
    assert_eq!(read_guard(&dir), GuardRead::Unreadable);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A status line of dstack's shape, its other keys and the site data it
/// carries (a transmitter's name, its source) passed over.
const STATUS: &str = r#"{"clock":{"ready":1,"epoch":1,"polls":9,"re_anchors":0,"holdovers":0,"holdover":0,"freq_ppb":-5,"source":"internal"},"requests":[],"tx":[{"flow":1,"receiver":"zyxqw-iemrack","live":1,"fpp":4,"channels":4,"tx_channels":[1,2,3,null],"packets":10,"late_bins":[1,0,0,0,0,0],"receiver_latency_us":250,"direct":1},{"flow":2,"live":0,"fpp":0,"channels":0,"packets":3,"receiver_latency_us":0}],"rx":[{"flow":1,"granted":0,"multicast":0,"fpp":0,"channels":0,"latency":0,"playout":0,"delay":0},{"flow":4,"device":"zyxqw-stage","source":"10.0.0.10:4321","granted":1,"multicast":0,"fpp":4,"channels":8,"rx_channels":[1,3],"latency":24,"playout":40,"delay":16},{"flow":24,"device":"zyxqw-radio","granted":1,"multicast":1,"fpp":32,"channels":2,"rx_channels":[2],"latency":72,"playout":72,"delay":48}],"channels":[{"rx":1,"status":16842761}],"engine":{"media":1,"offset":56,"playout":24,"blocks":5,"late":0,"missed":0,"overruns":0,"rebootstraps":0,"max_process_us":80}}"#;

#[test]
fn a_status_line_reads_as_its_flows() {
    let flows = parse_flows(STATUS).expect("the status reads");
    assert_eq!(flows.engine, Some(Engine { offset: Some(56) }));
    assert_eq!(flows.rx.len(), 3);
    assert_eq!(flows.rx[0].rx_channels, None);
    assert_eq!(
        flows.tx[0].tx_channels,
        Some(vec![Some(1), Some(2), Some(3), None])
    );
    assert_eq!(flows.tx[1].tx_channels, None);
    let j = judge(&box_guard(), Some(&flows));
    assert_eq!(j.line, line(3, 3, 0, 0, Some(128)));
    // A status without flows or an engine reads as none of them.
    assert_eq!(parse_flows(r#"{"clock":{}}"#), Some(Flows::default()));
    for bad in [
        "",
        "error: no such request",
        r#"{"rx":[{"delay":"x"}]}"#,
        r#"{"tx":{}}"#,
    ] {
        assert_eq!(parse_flows(bad), None, "{bad}");
    }
}

#[test]
fn assess_skips_without_the_file_and_reads_the_shown_line() {
    assert_eq!(assess(&GuardRead::Absent, Some(STATUS)), (None, vec![]));
    assert_eq!(
        assess(&GuardRead::Unreadable, Some(STATUS)),
        (Some(Line::default()), vec![Reason::LatencyUnread])
    );
    let read = GuardRead::Read(box_guard());
    assert_eq!(
        assess(&read, Some(STATUS)),
        (Some(line(3, 3, 0, 0, Some(128))), vec![])
    );
    // No read at all: health's own `no_status`, the guard's numbers unknown.
    assert_eq!(assess(&read, None), (Some(line(3, 0, 0, 0, None)), vec![]));
    assert_eq!(
        assess(&read, Some("{\"rx\":7}")),
        (Some(line(3, 0, 0, 0, None)), vec![Reason::LatencyUnread])
    );
    let over = GuardRead::Read(guard(&[(&[2], 96)]));
    assert_eq!(assess(&over, Some(STATUS)).1, [Reason::LatencyOverBudget]);
}

#[test]
fn the_line_is_numbers_or_null() {
    assert_eq!(
        serde_json::to_string(&line(3, 2, 1, 1, Some(128))).unwrap(),
        "{\"inputs\":3,\"known\":2,\"over\":1,\"unread_tx\":1,\"worst_samples\":128,\"budget_samples\":96}"
    );
    assert_eq!(
        serde_json::to_string(&Line::default()).unwrap(),
        "{\"inputs\":0,\"known\":0,\"over\":0,\"unread_tx\":0,\"worst_samples\":null,\"budget_samples\":null}"
    );
}
