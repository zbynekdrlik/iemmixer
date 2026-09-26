//! Newline-separated client messages through `parse_client` and
//! `Core::apply` on the test site: no input may panic, and every accepted
//! command's RT group fits one block (an import is split by the control loop).
#![no_main]

use std::sync::{Arc, OnceLock};

use iem_engine::MAX_CMDS_PER_BLOCK;
use iem_engine::core::{Core, Flags};
use iem_engine::site::parse;
use iem_engine::topology::{Topology, compile};
use iem_engine_proto::{ClientMsg, Cmd, MixState, parse_client};
use libfuzzer_sys::fuzz_target;

static TOPOLOGY: OnceLock<Arc<Topology>> = OnceLock::new();

fuzz_target!(|data: &[u8]| {
    let topo = TOPOLOGY.get_or_init(|| {
        let site = parse(include_str!("../../config/test-site.toml")).expect("test site");
        Arc::new(compile(&site).expect("test topology"))
    });
    let flags = Flags {
        test_signal: true,
        fault_injection: true,
    };
    let mut core = Core::new(Arc::clone(topo), &MixState::default(), 0, flags);
    for line in data.split(|b| *b == b'\n') {
        if let Ok(ClientMsg::Request { cmd, .. }) = parse_client(line) {
            let before = core.rev();
            match core.apply(&cmd) {
                Ok(out) => {
                    assert!(out.rev == before || out.rev == before + 1);
                    if !matches!(cmd, Cmd::ImportState { .. }) {
                        assert!(out.rt.len() <= MAX_CMDS_PER_BLOCK);
                    }
                }
                Err(_) => assert_eq!(core.rev(), before),
            }
        }
    }
});
