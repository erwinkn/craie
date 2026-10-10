//! The shared estimate fixture (`packages/bridge/traces/estimates.json`):
//! native templates estimate as the kit's do, whatever the band order,
//! and `LIST_CONFIG2` accepts every template the fixture holds.

use craie_harness::json::{self, Value};
use craie_ui::mutation::{NIL, NodeKind, Template, Transaction};
use craie_ui::ui::Ui;
use craie_ui::wire;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../packages/bridge/traces/estimates.json"
);

fn num(v: &Value, key: &str) -> f32 {
    v.get(key)
        .and_then(Value::num)
        .unwrap_or_else(|| panic!("{key}")) as f32
}

/// A fixture template as the bridge sends it (`charWidth` defaults to
/// 0.5).
fn template(t: &Value) -> Template {
    match t.get("kind").and_then(Value::str) {
        Some("fixed") => Template::Fixed(num(t, "size")),
        Some("widths") => Template::Widths(
            (t.get("bands").map_or(&[][..], Value::arr).iter())
                .map(|b| {
                    (
                        b.arr()[0].num().unwrap() as f32,
                        b.arr()[1].num().unwrap() as f32,
                    )
                })
                .collect(),
        ),
        Some("text") => Template::Text {
            base: num(t, "base"),
            inset: num(t, "inset"),
            font_size: num(t, "fontSize"),
            line_height: num(t, "lineHeight"),
            char_width: t.get("charWidth").and_then(Value::num).unwrap_or(0.5) as f32,
        },
        k => panic!("template kind {k:?}"),
    }
}

#[test]
fn templates_estimate_as_the_fixture_says() {
    let doc = json::parse(&std::fs::read_to_string(FIXTURE).unwrap()).unwrap();
    assert_eq!(
        doc.get("format").and_then(Value::str),
        Some("craie-list-estimates/1")
    );
    let cases = doc.get("cases").map_or(&[][..], Value::arr);
    assert!(cases.len() >= 28);
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::List).append(NIL, 0);
    ui.apply_txn(&t).unwrap();
    for (k, case) in cases.iter().enumerate() {
        let name = case.get("name").and_then(Value::str).unwrap();
        let tpl = template(case.get("template").unwrap());
        let len = case.get("textLength").and_then(Value::num).unwrap_or(0.0) as u32;
        let size = case.get("size").and_then(Value::num).map(|s| s as f32);
        assert_eq!(tpl.estimate(len, num(case, "width")), size, "{name}");
        // The template configures a list through the wire, bands in
        // any order.
        let mut t = Transaction::new(2 + k as u64);
        t.list_config2(0, 600.0, -1.0, 3.0, 48.0, 0, &[tpl]);
        (ui.apply(&wire::encode(&t))).unwrap_or_else(|e| panic!("{name}: {e:?}"));
    }
}
