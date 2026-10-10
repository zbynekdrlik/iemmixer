//! HIL's requests (design §7, §10): its jobs, the test signal on the
//! spare card outputs, the bundle's result, the fault injections and the
//! idle runner's stop.

use super::requests::outcome;
use super::{Guard, HIL_MAX_DBFS, HIL_MAX_TTL_S};
use crate::bundle::Hil;
use crate::pc::Pc;

/// The HIL test signal, card-masked to `[guard] hil_tx` (design §4), only
/// inside a begun HIL job (design §7); `listen` adds the listen probe (S7,
/// #10) behind the same gates.
pub(super) fn test_signal(
    pc: &mut dyn Pc,
    g: &mut Guard,
    input: &str,
    dbfs: f64,
    ttl_s: f64,
    listen: bool,
) -> (bool, String) {
    if let Err(why) = g.need_dev("test-signal") {
        return (false, why);
    }
    if g.state.job.is_none() {
        return (
            false,
            "a test signal needs a begun HIL job (job-begin)".to_owned(),
        );
    }
    if g.site.hil_tx.is_empty() {
        return (
            false,
            "[guard] hil_tx is empty: no card output may carry a test signal".to_owned(),
        );
    }
    if dbfs.is_nan() || dbfs > HIL_MAX_DBFS {
        return (
            false,
            format!("{dbfs} dBFS is above the HIL ceiling of {HIL_MAX_DBFS} dBFS"),
        );
    }
    if !ttl_s.is_finite() || ttl_s <= 0.0 {
        return (false, format!("a TTL of {ttl_s} s is not a positive time"));
    }
    if ttl_s > HIL_MAX_TTL_S {
        return (
            false,
            format!("a TTL of {ttl_s} s is above the HIL limit of {HIL_MAX_TTL_S} s"),
        );
    }
    let tx = g.site.hil_tx.clone();
    let probe = if listen { "; listen probe" } else { "" };
    outcome(
        pc.engine_hil_signal(input, dbfs, ttl_s, &tx, listen),
        &format!(
            "test signal on {input} at {dbfs} dBFS for {ttl_s} s on card outputs {tx:?}{probe}"
        ),
    )
}

/// `report <sha> <green|red> <detail>`: the bundle's HIL result.
pub(super) fn report(g: &mut Guard, sha: &str, hil: &str, detail: &str) -> (bool, String) {
    let result = match hil {
        "green" => Hil::Green,
        "red" => Hil::Red,
        other => return (false, format!("HIL result {other:?}: green or red")),
    };
    let Some(rec) = g.state.bundles.get_mut(sha) else {
        return (false, format!("bundle {sha} is not installed"));
    };
    rec.hil = result;
    g.save();
    (true, format!("bundle {sha}: HIL {hil} ({detail})"))
}

/// A HIL job begins in dev while no other job runs (a switch in progress
/// refuses it at the pipe, `while_switching`). Nothing reads the stage: only
/// the owner's signal decides whether the PC may be used, and other devices
/// on the Dante network feed the card's inputs (#38, owner 2026-10-06).
pub(super) fn job_begin(g: &mut Guard, run: u64) -> (bool, String) {
    if let Err(why) = g.need_dev("a HIL job") {
        return (false, why);
    }
    if let Some(other) = g.state.job {
        return (false, format!("HIL job {other} has not ended"));
    }
    g.state.job = Some(run);
    g.save();
    (true, format!("HIL job {run} began"))
}

pub(super) fn job_end(g: &mut Guard, run: u64) -> (bool, String) {
    match g.state.job {
        Some(r) if r == run => {
            g.state.job = None;
            g.save();
            (true, format!("HIL job {run} ended"))
        }
        Some(r) => (false, format!("HIL job {r} runs, not {run}")),
        None => (true, format!("no HIL job runs (job {run} has ended)")),
    }
}

/// The gate of every fault injection (HIL, design §7 and §10): dev first
/// (never live, whatever job is recorded), then a begun HIL job, whose
/// engine runs with its fault-injection flag (the engine refuses the
/// injection otherwise). `what` is the iemmode word, `test` names the test
/// in the refusal.
fn in_hil_job(g: &Guard, what: &str, test: &str) -> Result<(), String> {
    g.need_dev(what)?;
    if g.state.job.is_none() {
        return Err(format!("{test} needs a begun HIL job (job-begin)"));
    }
    Ok(())
}

/// HIL's RT panic (design §7): dev, inside a begun HIL job, forwarded to
/// the engine (started with its fault-injection flag for the job, which
/// refuses it otherwise); its exit 70 is the watch's, which starts it again
/// after the backoff (`crash::after_exit`, the fade-in on the new stream).
pub(super) fn inject_fault(pc: &mut dyn Pc, g: &mut Guard) -> (bool, String) {
    if let Err(why) = in_hil_job(g, "inject-fault", "a fault") {
        return (false, why);
    }
    outcome(
        pc.engine_inject_fault(),
        "the engine faults its RT callback; the watch starts it again",
    )
}

/// The owner-approved SEH test (design §10 test #4): dev, inside a begun
/// HIL job, forwarded to the engine (started with its fault-injection flag
/// for the job). The engine raises a structured exception on its RT
/// callback; the SEH filter releases the driver within its bound or parks
/// the stream, and the watch starts the engine again.
pub(super) fn inject_seh(pc: &mut dyn Pc, g: &mut Guard) -> (bool, String) {
    if let Err(why) = in_hil_job(g, "inject-seh", "an SEH test") {
        return (false, why);
    }
    outcome(
        pc.engine_inject_seh(),
        "the engine raises a structured exception on its RT callback; \
         the SEH filter releases the driver or parks, and the watch starts it again",
    )
}

/// The parked-engine test (design §10 test #2, #35): dev, inside a begun
/// HIL job, forwarded to the engine (started with its fault-injection flag
/// for the job). The engine raises the SEH test's exception under its
/// backend's test hold: the driver is kept, the SEH filter parks the RT
/// thread, and the engine keeps running with its stream parked and the card
/// held (`Status.parked`, so `iemmode status`) until it ends: test #2 ends
/// it with an OS restart, any `Shutdown` (an "ide event", a job's engine
/// restart) too. The watch sees no exit, so it starts nothing; the event
/// plan's `EngineStop` meets the parked engine as after a stuck callback
/// (R6).
pub(super) fn inject_park(pc: &mut dyn Pc, g: &mut Guard) -> (bool, String) {
    if let Err(why) = in_hil_job(g, "inject-park", "a parked-engine test") {
        return (false, why);
    }
    outcome(
        pc.engine_inject_park(),
        "the engine raises a structured exception under the test hold: its stream \
         parks with the card held and the engine keeps running until it ends \
         (test #2 ends it with an OS restart)",
    )
}

/// Ctrl-Break to the idle runner (bootstrap check).
pub(super) fn runner_stop(pc: &mut dyn Pc, g: &mut Guard) -> (bool, String) {
    if let Err(why) = g.need_dev("runner-stop") {
        return (false, why);
    }
    if let Some(run) = g.state.job {
        return (false, format!("HIL job {run} runs: the runner is not idle"));
    }
    let c = g.cancel.clone();
    outcome(pc.runner_stop(&c), "the runner stopped")
}
