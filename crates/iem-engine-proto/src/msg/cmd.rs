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
}

/// Every `op` tag, for telling an unknown command from a malformed one.
pub const OPS: [&str; 22] = [
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
    "ping",
    "arm",
    "hil_test_signal",
];

impl Cmd {
    /// Commands an observer connection may send.
    pub fn is_read_only(&self) -> bool {
        matches!(self, Self::GetState | Self::GetTopology | Self::Ping)
    }

    /// Commands only the supervisor may send (S6 design note §4): `Arm`,
    /// the HIL signal and the fault injection (review of PR #40, #35);
    /// anyone else gets `NotSupervisor`.
    pub fn is_supervisor(&self) -> bool {
        matches!(
            self,
            Self::Arm | Self::HilTestSignal { .. } | Self::InjectFault
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
