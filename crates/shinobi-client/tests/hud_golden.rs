//! HUD layout goldens: each fixed scenario's draw list is dumped to text and compared with `tests/golden/hud_<name>.txt`.
//! Intentional HUD changes: `UPDATE_GOLDEN=1 cargo test -p shinobi-client --test hud_golden` then review the diff
//! (and look at the images: `cargo run -p shinobi-client --example hud_dump -- /tmp/hud`).
use shinobi_client::hud_scenarios::{dump, scenarios};

#[test]
fn hud_scenarios_match_their_golden_layouts() {
    let update = std::env::var("UPDATE_GOLDEN").is_ok();
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden");
    for (name, prims) in scenarios() {
        let path = format!("{dir}/hud_{name}.txt");
        let got = dump(&prims);
        if update {
            std::fs::write(&path, &got).unwrap();
            continue;
        }
        let want = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing golden {path}: run with UPDATE_GOLDEN=1"));
        if got != want {
            let first = got.lines().zip(want.lines()).position(|(a, b)| a != b).map_or_else(|| got.lines().count().min(want.lines().count()), |i| i);
            panic!(
                "HUD scenario '{name}' changed (first difference at line {}):\n  got : {}\n  want: {}\nIf intended: UPDATE_GOLDEN=1 cargo test -p shinobi-client --test hud_golden",
                first + 1,
                got.lines().nth(first).unwrap_or("<end>"),
                want.lines().nth(first).unwrap_or("<end>")
            );
        }
    }
}
