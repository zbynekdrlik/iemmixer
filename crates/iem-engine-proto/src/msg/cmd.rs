//! The commands (`op`-tagged) and which role may send each.

use serde::{Deserialize, Serialize};

use crate::ids::{EqTarget, GroupId, InputId, MixId, Source};
use crate::state::{Eq, MixState};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Cmd {
    /// F29: trim, mute and processing of an input (its EQ: `SetEq`).
    SetInput {
        input: InputId,
        trim_db: Option<f64>,
        muted: Option<bool>,
        processing: Option<bool>,
    },
    /// F7: a mix's volume and mute (its EQ: `SetEq`, its limiter: `SetLimiter`).
    SetMix {
        mix: MixId,
        volume_db: Option<f64>,
        muted: Option<bool>,
    },
    /// F5, F16: the level of an input or a heard mix in a mix.
    SetLevel {
        mix: MixId,
        source: Source,
        gain_db: Option<f64>,
        pan: Option<f64>,
        muted: Option<bool>,
    },
    /// F7: a group's strip in a mix (its EQ: `SetEq`).
    SetGroup {
        mix: MixId,
        group: GroupId,
        gain_db: Option<f64>,
        muted: Option<bool>,
    },
    SetEq {
        target: EqTarget,
        eq: Eq,
    },
    SetLimiter {
        mix: MixId,
        enabled: Option<bool>,
        limit_db: Option<f64>,
    },
    ResetLimiterStats {
        mix: MixId,
    },
    SetSolo {
        mix: MixId,
        sources: Vec<Source>,
    },
    StartListen {
        mix: MixId,
    },
    StopListen {
        mix: MixId,
    },
    StartTestSignal {
        input: InputId,
        hz: f64,
        dbfs: f64,
        ttl_s: f64,
    },
    StopTestSignal,
    Batch {
        ops: Vec<Cmd>,
    },
    ImportState {
        state: MixState,
        #[serde(default)]
        baseline: bool,
    },
    GetState,
    GetTopology,
    SaveNow,
    Shutdown,
    /// A panic on the RT thread (HIL's panic check), under the
    /// fault-injection flag. Supervisor only: an engine keeps the flag until
    /// it restarts, after its HIL job too, and only the guard allows an
    /// injection, inside a job (review of PR #40, #35).
    InjectFault,
    /// Owner-approved SEH test (design §10): a structured exception on the
    /// RT thread, under the fault-injection flag. Supervisor only, like
    /// `InjectFault`.
    InjectSeh,
    /// The parked-engine test (design §10 test #2, #35): the SEH test's
    /// exception under a test hold, so the backend keeps the driver and the
    /// SEH filter parks the RT thread; the engine keeps running and reports
    /// `parked`. Under the fault-injection flag, supervisor only, like
    /// `InjectSeh`.
    InjectPark,
    Ping,
    /// The supervisor lets an engine started with `--hold` sound (S6 design
    /// note §4): the outputs fade in over 500 ms.
    Arm,
    /// The HIL test signal (S6 design note §4, §7): the test signal on
    /// `input` (under the test-signal flag, capped like `StartTestSignal`;
    /// `dbfs` above the cap is refused), whose sine, while it runs, sounds
    /// only on the card outputs `card_tx`: spare outputs no mix uses (the
    /// site's `[guard] hil_tx`, which the engine opens under the flag); a
    /// mix's TX is refused, so it never reaches a band member. Every mix's
    /// TX stays zero meanwhile; the mixes render as usual, so their meters
    /// show the routing. Supervisor only.
    HilTestSignal {
        input: InputId,
        hz: f64,
        dbfs: f64,
        ttl_s: f64,
        card_tx: Vec<u16>,
        /// S7, additive (design note §6): the listen probe. While the signal
        /// runs, the listen taps keep their silent frames and the probe streams
        /// (`media::stream::ENGINEER_PROBE`, `MEMBER_PROBE`) carry the spare
        /// outputs' sine, for the server's `&hil=1` listeners only. Every mix's
        /// TX stays zero. An older engine ignores it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        listen: bool,
    },
    /// HIL's forced reopen of the driver (S6 design note §7): the backend
    /// stops, releases and opens the card again (its reset budget applies;
    /// the outputs fade in again). Supervisor only, under the
    /// fault-injection flag.
    ForceReopen,
}

/// Every `op` tag, for telling an unknown command from a malformed one.
pub const OPS: [&str; 25] = [
    "set_input",
    "set_mix",
    "set_level",
    "set_group",
    "set_eq",
    "set_limiter",
    "reset_limiter_stats",
    "set_solo",
    "start_listen",
    "stop_listen",
    "start_test_signal",
    "stop_test_signal",
    "batch",
    "import_state",
    "get_state",
    "get_topology",
    "save_now",
    "shutdown",
    "inject_fault",
    "inject_seh",
    "inject_park",
    "ping",
    "arm",
    "hil_test_signal",
    "force_reopen",
];

impl Cmd {
    /// Commands an observer connection may send.
    pub fn is_read_only(&self) -> bool {
        matches!(self, Self::GetState | Self::GetTopology | Self::Ping)
    }

    /// Commands only the supervisor may send (S6 design note §4): `Arm`,
    /// the HIL signal, the forced reopen and the fault injections (review
    /// of PR #40, #35); anyone else gets `NotSupervisor`.
    pub fn is_supervisor(&self) -> bool {
        matches!(
            self,
            Self::Arm
                | Self::HilTestSignal { .. }
                | Self::ForceReopen
                | Self::InjectFault
                | Self::InjectSeh
                | Self::InjectPark
        )
    }

    /// Commands a supervisor connection may send: reads, its own commands
    /// (fault injection among them), `Shutdown`, `SaveNow` and the test
    /// signal (their launch flags still apply); never a change of the mix.
    pub fn supervisor_may(&self) -> bool {
        self.is_read_only()
            || self.is_supervisor()
            || matches!(
                self,
                Self::Shutdown
                    | Self::SaveNow
                    | Self::StartTestSignal { .. }
                    | Self::StopTestSignal
            )
    }
}
