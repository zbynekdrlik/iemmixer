//! One request after another (`Core::apply`, `Core::batch`): each command's
//! checks and caps, the entity it sets through the per-entity setters, its
//! effect and its RT commands; the test signal (X13) and the HIL signal (S6)
//! start here.

use iem_engine_proto::{
    Change, Cmd, EqTarget, ErrCode, InputId, InputState, MixGroup, MixOut, TestSignal, db_to_lin,
};

use super::{CmdError, Core, Effect, Partial, capped, clip, hil_mask, ix, reconcile};
use crate::SAMPLE_RATE;
use crate::cmd::{HilMask, RtOp};
use crate::params::{
    FADER_DB, LIMIT_DB, PAN, TEST_DBFS, TEST_HZ, TEST_TTL_S, TRIM_DB, cap_eq, eq_is_finite,
};

impl Core {
    pub(super) fn one(&mut self, cmd: &Cmd) -> Result<Partial, CmdError> {
        match cmd {
            Cmd::SetInput {
                input,
                trim_db,
                muted,
                processing,
            } => {
                let i = self.input(input)?;
                let mut new = self.s.inputs.get(i).copied().unwrap_or_default();
                if let Some(v) = trim_db {
                    new.trim_db = capped(*v, TRIM_DB, "trim_db")?;
                }
                if let Some(v) = muted {
                    new.muted = *v;
                }
                if let Some(v) = processing {
                    new.processing = *v;
                }
                Ok(self.set_input(i, new))
            }
            Cmd::SetMix {
                mix,
                volume_db,
                muted,
            } => {
                let m = self.mix(mix)?;
                let mut new = self.out(m);
                if let Some(v) = volume_db {
                    new.volume_db = capped(*v, FADER_DB, "volume_db")?;
                }
                if let Some(v) = muted {
                    new.muted = *v;
                }
                Ok(self.set_out(m, new))
            }
            Cmd::SetLevel {
                mix,
                source,
                gain_db,
                pan,
                muted,
            } => {
                let m = self.mix(mix)?;
                let k = self.topo.slot(m, source).ok_or_else(|| {
                    CmdError::new(
                        ErrCode::UnknownId,
                        format!(
                            "mix {} does not hear {}",
                            clip(&mix.0),
                            clip(&source.to_string())
                        ),
                    )
                })?;
                let mut new = self.level(m, k);
                if let Some(v) = gain_db {
                    new.gain_db = capped(*v, FADER_DB, "gain_db")?;
                }
                if let Some(v) = pan {
                    new.pan = capped(*v, PAN, "pan")?;
                }
                if let Some(v) = muted {
                    new.muted = *v;
                }
                Ok(self.set_level(m, k, new))
            }
            Cmd::SetGroup {
                mix,
                group,
                gain_db,
                muted,
            } => {
                let m = self.mix(mix)?;
                let g = self.group(group)?;
                let mut new = self.strip(m, g);
                if let Some(v) = gain_db {
                    new.gain_db = capped(*v, FADER_DB, "gain_db")?;
                }
                if let Some(v) = muted {
                    new.muted = *v;
                }
                Ok(self.set_group(m, g, new))
            }
            Cmd::SetEq { target, eq } => {
                if !eq_is_finite(eq) {
                    return Err(CmdError::new(
                        ErrCode::BadValue,
                        "an EQ value is not finite",
                    ));
                }
                let eq = cap_eq(eq);
                match target {
                    EqTarget::Input(id) => {
                        let i = self.input(id)?;
                        let old = self.s.inputs.get(i).copied().unwrap_or_default();
                        Ok(self.set_input(i, InputState { eq, ..old }))
                    }
                    EqTarget::Mix(id) => {
                        let m = self.mix(id)?;
                        let old = self.out(m);
                        Ok(self.set_out(m, MixOut { eq, ..old }))
                    }
                    EqTarget::Group { mix, group } => {
                        let m = self.mix(mix)?;
                        let g = self.group(group)?;
                        let old = self.strip(m, g);
                        Ok(self.set_group(m, g, MixGroup { eq, ..old }))
                    }
                }
            }
            Cmd::SetLimiter {
                mix,
                enabled,
                limit_db,
            } => {
                let m = self.mix(mix)?;
                let mut new = self.out(m);
                if let Some(v) = limit_db {
                    new.limiter.limit_db = capped(*v, LIMIT_DB, "limit_db")?;
                }
                if let Some(v) = enabled {
                    new.limiter.enabled = *v;
                }
                Ok(self.set_out(m, new))
            }
            Cmd::ResetLimiterStats { mix } => {
                let m = self.mix(mix)?;
                Ok(Partial::changed(
                    vec![Change::LimiterStatsReset { mix: mix.clone() }],
                    vec![RtOp::ResetLimiter { m: ix(m) }],
                ))
            }
            Cmd::SetSolo { mix, sources } => self.set_solo(mix, sources),
            Cmd::StartListen { mix } => self.start_listen(mix),
            Cmd::StopListen { mix } => {
                let m = self.mix(mix)?;
                let mut rt = Vec::new();
                for (slot, l) in self.listen.iter_mut().enumerate() {
                    if *l == Some(m) {
                        *l = None;
                        rt.push(RtOp::Listen {
                            slot: slot as u8,
                            mix: None,
                        });
                    }
                }
                Ok(self.listen_partial(rt))
            }
            Cmd::StartTestSignal {
                input,
                hz,
                dbfs,
                ttl_s,
            } => {
                self.test_flag()?;
                if self.hil_test && self.test.is_some() {
                    return Err(CmdError::new(
                        ErrCode::Forbidden,
                        "a HIL test signal runs until its TTL ends",
                    ));
                }
                self.start_test(input, *hz, *dbfs, *ttl_s, None, false)
            }
            Cmd::HilTestSignal {
                input,
                hz,
                dbfs,
                ttl_s,
                card_tx,
                listen,
            } => {
                self.test_flag()?;
                // Refused above the X13 cap, never lowered: HIL checks the
                // level it asked for.
                if *dbfs > TEST_DBFS.1 {
                    return Err(CmdError::new(
                        ErrCode::BadValue,
                        "dbfs is above the test-signal cap of -20 dBFS",
                    ));
                }
                let mask = hil_mask(&self.topo, &self.hil, card_tx)?;
                self.start_test(input, *hz, *dbfs, *ttl_s, Some(mask), *listen)
            }
            Cmd::StopTestSignal => Ok(if self.test.take().is_some() {
                Partial::changed(
                    vec![Change::TestSignal { signal: None }],
                    vec![RtOp::StopTestSignal],
                )
            } else {
                Partial::default()
            }),
            Cmd::InjectFault => {
                self.need_fault_injection()?;
                Ok(Partial {
                    rt: vec![RtOp::Panic],
                    ..Partial::default()
                })
            }
            Cmd::InjectSeh => {
                self.need_fault_injection()?;
                Ok(Partial {
                    rt: vec![RtOp::Seh],
                    ..Partial::default()
                })
            }
            // The parked-engine test (design §10 test #2, #35): the SEH
            // test's exception under the backend's test hold.
            Cmd::InjectPark => {
                self.need_fault_injection()?;
                Ok(Partial {
                    rt: vec![RtOp::Park],
                    ..Partial::default()
                })
            }
            Cmd::ForceReopen => {
                self.need_fault_injection()?;
                Ok(Partial::effect(Effect::Reopen))
            }
            Cmd::Batch { .. } => Err(CmdError::new(ErrCode::BadRequest, "nested batch")),
            Cmd::ImportState { state, baseline } => {
                self.s = reconcile(&self.topo, state).0;
                Ok(Partial {
                    rt: self.full_sync(),
                    effect: Some(Effect::Imported {
                        baseline: *baseline,
                    }),
                    bump: true,
                    ..Partial::default()
                })
            }
            Cmd::GetState => Ok(Partial::effect(Effect::SendState)),
            Cmd::GetTopology => Ok(Partial::effect(Effect::SendTopology)),
            Cmd::SaveNow => Ok(Partial::effect(Effect::Save)),
            Cmd::Shutdown => Ok(Partial::effect(Effect::Shutdown)),
            Cmd::Ping => Ok(Partial::default()),
            // Neither state nor revision: the processor's fade-in starts.
            Cmd::Arm => Ok(Partial {
                rt: vec![RtOp::Arm],
                ..Partial::default()
            }),
        }
    }

    fn test_flag(&self) -> Result<(), CmdError> {
        if self.flags.test_signal {
            Ok(())
        } else {
            Err(CmdError::new(
                ErrCode::Forbidden,
                "the engine runs without the test-signal flag",
            ))
        }
    }

    /// X13: a sine replaces `input` for `ttl_s`, every TX capped; with
    /// `mask` (the HIL signal) it sounds only on those spare outputs
    /// meanwhile, and no mix's TX carries anything; `listen` (S7, HIL only)
    /// adds the listen probe.
    fn start_test(
        &mut self,
        input: &InputId,
        hz: f64,
        dbfs: f64,
        ttl_s: f64,
        mask: Option<HilMask>,
        listen: bool,
    ) -> Result<Partial, CmdError> {
        let i = ix(self.input(input)?);
        let hz = capped(hz, TEST_HZ, "hz")?;
        let dbfs = capped(dbfs, TEST_DBFS, "dbfs")?;
        let ttl_s = capped(ttl_s, TEST_TTL_S, "ttl_s")?;
        let signal = TestSignal {
            input: input.clone(),
            hz,
            dbfs,
            ttl_s,
        };
        self.test = Some(signal.clone());
        self.hil_test = mask.is_some();
        let amp = db_to_lin(dbfs);
        let ttl = (ttl_s * f64::from(SAMPLE_RATE)).round() as u64;
        let op = match mask {
            None => RtOp::TestSignal { i, hz, amp, ttl },
            Some(mask) => RtOp::HilTestSignal {
                i,
                hz,
                amp,
                ttl,
                mask,
                listen,
            },
        };
        Ok(Partial::changed(
            vec![Change::TestSignal {
                signal: Some(signal),
            }],
            vec![op],
        ))
    }
}
