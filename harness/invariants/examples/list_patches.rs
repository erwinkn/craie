//! Streamed list edits at scale (the lists contract, protocol 20): the
//! cost of applying one transaction to a list of N items while rows are
//! tagged by item, as a streaming reply does it.
//!
//!   cargo run --release -p craie-harness --example list_patches
//!
//! Per transaction (decode, validate, apply; no layout), median and p95
//! over `RUNS`:
//!   - append: one patch appending one item;
//!   - append + update: one patch appending an item and giving the one
//!     before it a new version (the reply that grows while it streams);
//!   - append, update, re-tag: the same plus `LIST_ROW2` re-tagging the
//!     updated row at its new version.

use std::time::Instant;

use craie_ui::mutation::{Item, ListOp, NIL, NodeKind, Transaction};
use craie_ui::ui::Ui;
use craie_ui::wire;

const LIST: u32 = 1;
const ROWS: u32 = 30;
const FIRST_ROW: u32 = 100;
const RUNS: usize = 2000;

/// A list of `n` items with the last `ROWS` of them tagged by rows.
fn setup(n: u32) -> Ui {
    let mut ui = Ui::new(1.0);
    let items: Vec<Item> = (1..=n).map(|id| Item::sized(id, 0, 40.0)).collect();
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View).append(NIL, 0);
    t.create(LIST, NodeKind::List)
        .list_config2(LIST, 600.0, -1.0, 3.0, 48.0, 0, &[])
        .list_patch(LIST, 0, 1, &[ListOp::splice(0, 0, &items)])
        .append(0, LIST);
    for k in 0..ROWS {
        let row = FIRST_ROW + k;
        t.create(row, NodeKind::View)
            .list_row(row, LIST, n - ROWS + 1 + k, 0)
            .append(LIST, row);
    }
    ui.apply(&wire::encode(&t)).unwrap();
    ui
}

/// Median and p95 microseconds of applying `txn(i)` for i in 0..RUNS.
fn time(ui: &mut Ui, mut txn: impl FnMut(&Ui, usize) -> Transaction<'static>) -> (f64, f64) {
    let mut us: Vec<f64> = (0..RUNS)
        .map(|i| {
            let bytes = wire::encode(&txn(ui, i));
            let t = Instant::now();
            ui.apply(&bytes).unwrap();
            t.elapsed().as_secs_f64() * 1e6
        })
        .collect();
    us.sort_by(f64::total_cmp);
    (us[RUNS / 2], us[RUNS * 95 / 100])
}

fn revision(ui: &Ui) -> u32 {
    ui.host.lists.get(LIST).unwrap().revision
}

fn main() {
    println!("list patches: one transaction per streamed edit, {ROWS} tagged rows (us)");
    println!(
        "{:>9} {:>17} {:>17} {:>17}",
        "items", "append", "append+update", "+re-tag"
    );
    for n in [1_000, 10_000, 50_000, 200_000] {
        let cell = |(m, p): (f64, f64)| format!("{m:>7.1} p95 {p:>6.1}");
        let mut ui = setup(n);
        let append = time(&mut ui, |ui, i| {
            let (rev, id) = (revision(ui), n + 1 + i as u32);
            let mut t = Transaction::new(ui.seq + 1);
            let op = ListOp::splice(n + i as u32, 0, &[Item::sized(id, 0, 40.0)]);
            t.list_patch(LIST, rev, rev + 1, &[op]);
            t
        });
        let mut ui = setup(n);
        let streamed = |retag: bool| {
            move |ui: &Ui, i: usize| {
                let (rev, len) = (revision(ui), ui.host.lists.get(LIST).unwrap().len());
                let last = ui.host.lists.get(LIST).unwrap().items[len as usize - 1];
                let next = Item::sized(last.id, last.version + 1, 60.0);
                let mut t = Transaction::new(ui.seq + 1);
                let ops = [
                    ListOp::update(len - 1, &[next]),
                    ListOp::splice(len, 0, &[Item::sized(2 * n + 1 + i as u32, 0, 40.0)]),
                ];
                t.list_patch(LIST, rev, rev + 1, &ops);
                if retag {
                    t.list_row(FIRST_ROW + (i as u32 % ROWS), LIST, next.id, next.version);
                }
                t
            }
        };
        let both = time(&mut ui, streamed(false));
        let mut ui = setup(n);
        let retag = time(&mut ui, streamed(true));
        println!("{n:>9} {} {} {}", cell(append), cell(both), cell(retag));
    }
}
