//! Headless Spike E gate for transport-chunk-independent VT state.
//!
//! This deliberately exercises only the repository-owned renderer-independent
//! core. It is not evidence for the provenance-blocked renderer or device lane.

use choosh_core::vt::{Damage, ScreenSnapshot, VtEngine};

const ROWS: usize = 4;
const COLUMNS: usize = 16;
const SCROLLBACK: usize = 8;

// Synthetic bytes cover incomplete UTF-8, SGR, cursor addressing, alternate
// screen enter/leave, erase, CR/LF, and invalid UTF-8 replacement. They are
// repository-authored rather than imported from a provenance-blocked terminal.
const STREAM: &[u8] =
    b"main\r\n\x1b[1;31mred\x1b[0m \xc3\xa9\x1b[?1049hALT\x1b[2;3HZ\x1b[2K\x1b[?1049l\xff!";

#[derive(Debug, Eq, PartialEq)]
struct Observation {
    snapshot: ScreenSnapshot,
    damage: Vec<Damage>,
    scrollback_rows: usize,
}

fn observe(chunks: impl IntoIterator<Item = Vec<u8>>) -> Observation {
    let mut engine = VtEngine::new(ROWS, COLUMNS, SCROLLBACK).unwrap();
    // Constructor damage is independent of input delivery and is consumed so
    // the gate compares only damage caused by the stream.
    let _ = engine.take_damage();
    for chunk in chunks {
        engine.feed(&chunk);
    }
    Observation {
        snapshot: engine.snapshot(),
        damage: engine.take_damage(),
        scrollback_rows: engine.scrollback_rows(),
    }
}

#[test]
fn identical_stream_is_invariant_at_every_byte_boundary() {
    let expected = observe([STREAM.to_vec()]);

    for boundary in 0..=STREAM.len() {
        let actual = observe([STREAM[..boundary].to_vec(), STREAM[boundary..].to_vec()]);
        assert_eq!(
            actual, expected,
            "state differed at byte boundary {boundary}"
        );
    }

    let byte_at_a_time = observe(STREAM.iter().map(|byte| vec![*byte]));
    assert_eq!(byte_at_a_time, expected, "byte-at-a-time state differed");
}
