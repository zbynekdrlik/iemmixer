//! What the S1a spike does with the glitches it drains from a stream (S1c
//! design note §4.1): the segment report's capped list and one trace marker
//! per glitch. `scripts/pc-tuning/latency_report.py` parses the marker text
//! to place each glitch on the trace's clock. Portable and mutated; the
//! spike's Windows calls (`os::qpc`, `os::Markers`) stay in the example.

use crate::telemetry::Glitch;

/// The most glitches one segment's report lists; more are only counted.
pub const GLITCH_REPORT_CAP: usize = 10_000;

/// Adds `new` to a segment's list up to [`GLITCH_REPORT_CAP`]; returns how
/// many did not fit.
pub fn keep_glitches(list: &mut Vec<Glitch>, new: &[Glitch]) -> usize {
    let take = new.len().min(GLITCH_REPORT_CAP.saturating_sub(list.len()));
    list.extend(new.iter().take(take).copied());
    new.len() - take
}

/// A glitch's QPC count: the stream's QPC `base` (read together with the
/// stream clock's zero) plus its stream-clock time `at_ns` at `freq` counts
/// per second. A negative frequency counts as 0; the sum saturates.
pub fn glitch_qpc(at_ns: u64, base: i64, freq: i64) -> i64 {
    let ticks = u128::from(at_ns) * u128::try_from(freq).unwrap_or(0) / 1_000_000_000;
    base.saturating_add(i64::try_from(ticks).unwrap_or(i64::MAX))
}

/// The trace marker of one glitch, as `latency_report.py` parses it: the
/// kind, the glitch's QPC, the QPC when the marker was written (`emit`), the
/// frequency and the glitch's value.
pub fn marker_text(g: &Glitch, base: i64, freq: i64, emit: i64) -> String {
    format!(
        "iemmixer-glitch kind={} at_qpc={} emit_qpc={emit} freq={freq} value={}",
        g.kind.name(),
        glitch_qpc(g.at_ns, base, freq),
        g.value
    )
}

/// Writes one marker per glitch through `write`, oldest first, with the QPC
/// count `now` returns as the marker's emit time.
pub fn write_markers(
    glitches: &[Glitch],
    base: i64,
    freq: i64,
    mut now: impl FnMut() -> i64,
    mut write: impl FnMut(&str),
) {
    let emit = now();
    for g in glitches {
        write(&marker_text(g, base, freq, emit));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::GlitchKind;

    fn glitch(kind: GlitchKind, at_ns: u64, value: u64) -> Glitch {
        Glitch { kind, at_ns, value }
    }

    #[test]
    fn the_report_list_is_capped_and_the_rest_counted() {
        let mut list = vec![glitch(GlitchKind::Late, 0, 1); GLITCH_REPORT_CAP - 2];
        let new = [glitch(GlitchKind::Missed, 1, 2); 5];
        assert_eq!(keep_glitches(&mut list, &new), 3);
        assert_eq!(list.len(), GLITCH_REPORT_CAP);
        assert_eq!(list[GLITCH_REPORT_CAP - 3].kind, GlitchKind::Late);
        assert_eq!(list[GLITCH_REPORT_CAP - 2..], [new[0], new[1]]);
        // A full list takes nothing more.
        assert_eq!(keep_glitches(&mut list, &new), 5);
        assert_eq!(list.len(), GLITCH_REPORT_CAP);
        // Below the cap everything fits, in order.
        let mut empty = Vec::new();
        assert_eq!(keep_glitches(&mut empty, &new), 0);
        assert_eq!(empty, new);
        assert_eq!(keep_glitches(&mut empty, &[]), 0);
        assert_eq!(empty.len(), 5);
    }

    #[test]
    fn glitch_times_convert_from_stream_ns_to_qpc_counts() {
        // 1 s at 10 MHz is 10_000_000 counts after the base.
        assert_eq!(glitch_qpc(1_000_000_000, 100, 10_000_000), 10_000_100);
        // One period at 32 samples / 96 kHz: the division truncates.
        assert_eq!(glitch_qpc(333_333, 0, 10_000_000), 3_333);
        assert_eq!(glitch_qpc(333_333, -5, 3_000_000_000), 999_994);
        // No usable frequency: the base alone.
        assert_eq!(glitch_qpc(5, 7, 0), 7);
        assert_eq!(glitch_qpc(5, 7, -1), 7);
        // Far beyond an i64 of counts: saturates, never wraps.
        assert_eq!(glitch_qpc(u64::MAX, 0, i64::MAX), i64::MAX);
        assert_eq!(glitch_qpc(1_000_000_000, i64::MAX - 1, 10), i64::MAX);
    }

    #[test]
    fn a_marker_carries_the_fields_latency_report_parses() {
        assert_eq!(
            marker_text(
                &glitch(GlitchKind::Missed, 1_000_000_000, 700_000),
                100,
                10_000_000,
                10_050_000
            ),
            "iemmixer-glitch kind=missed at_qpc=10000100 emit_qpc=10050000 freq=10000000 value=700000"
        );
        assert_eq!(
            marker_text(&glitch(GlitchKind::Overrun, 0, 400_000), -3, 1, 9),
            "iemmixer-glitch kind=overrun at_qpc=-3 emit_qpc=9 freq=1 value=400000"
        );
    }

    #[test]
    fn markers_are_written_oldest_first_and_none_without_glitches() {
        let glitches = [
            glitch(GlitchKind::Late, 1_000_000_000, 500_001),
            glitch(GlitchKind::PositionGap, 2_000_000_000, 64),
        ];
        let mut written = Vec::new();
        write_markers(&glitches, 0, 10, || 77, |t| written.push(t.to_owned()));
        assert_eq!(
            written,
            [
                "iemmixer-glitch kind=late at_qpc=10 emit_qpc=77 freq=10 value=500001",
                "iemmixer-glitch kind=position-gap at_qpc=20 emit_qpc=77 freq=10 value=64",
            ]
        );
        let mut none = 0;
        write_markers(&[], 0, 10, || 77, |_| none += 1);
        assert_eq!(none, 0);
    }
}
