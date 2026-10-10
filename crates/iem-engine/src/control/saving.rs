//! Saving the state (design note §3.6): the scheduled save, the
//! baseline, and their alarms.

use std::time::{SystemTime, UNIX_EPOCH};

use iem_engine_proto::{AlarmCode, EngineMsg};
use tracing::warn;

use super::Control;
use crate::persist::Persisted;

pub(super) fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

impl Control {
    fn persisted(&self) -> Persisted {
        let topo = self.core.topology();
        Persisted {
            rev: self.core.rev(),
            topology_hash: topo.hash.clone(),
            saved_unix_ms: unix_ms(),
            state: self.core.state(),
            counters: topo
                .mixes
                .iter()
                .zip(&self.counters)
                .map(|(b, c)| (b.id.clone(), *c))
                .collect(),
        }
    }

    pub(super) fn save(&mut self) {
        self.schedule.saved();
        match self.store.save(&self.persisted()) {
            Ok(committed) => {
                // The save stands; only old generations stayed (#32 P6).
                if let Some(why) = &committed.pruning {
                    warn!("old generations were not removed: {why}");
                }
                // #32 MAJOR-1: kept, never loaded; the engineer hears where.
                if let Some(aside) = &committed.orphaned {
                    self.alarm(
                        AlarmCode::StateFallback,
                        format!(
                            "a save.tmp the boot did not load was moved aside to {}",
                            aside.display()
                        ),
                    );
                }
                let rev = self.core.rev();
                self.broadcast(&EngineMsg::Saved {
                    rev,
                    generation: committed.generation,
                });
            }
            Err(e) => self.alarm(
                AlarmCode::SaveFailed,
                format!("saving the state failed: {e}"),
            ),
        }
    }

    pub(super) fn save_baseline(&mut self) {
        if let Err(e) = self.store.save_baseline(&self.persisted()) {
            self.alarm(
                AlarmCode::SaveFailed,
                format!("saving the baseline failed: {e}"),
            );
        }
    }
}
