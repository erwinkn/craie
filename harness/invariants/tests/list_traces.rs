//! The shared list traces (`docs/contracts/list-traces.md`) against
//! native lists: every file in `packages/bridge/traces/lists`, played
//! through `Ui` as the bridge would drive it. Rows mount for the range
//! the list reports, tagged by item and version, at the trace's
//! heights; the expectations read `Ui::list_viewport`.
//!
//! A trace in `PENDING` names native behavior not built yet: it must
//! still fail, so one that starts passing leaves the list.

use std::collections::{BTreeMap, HashMap};

use craie_core::geom::Size;
use craie_ui::events::{list_slot, mask, out_kind};
use craie_ui::host::NodeId;
use craie_ui::mutation::{
    Align, Anchor, Command, Item, Jump, ListOp, ListPolicy, NIL, NodeKind, Template, Transaction,
};
use craie_ui::ui::Ui;

/// Traces waiting for native work, with what they need.
const PENDING: &[(&str, &str)] = &[
    // Its step-3 offset pins how far above the viewport rows mount; #45
    // drops it (native lands at 8160 with the reader held).
    (
        "L1",
        "an overscan-dependent offset (until #45 reaches this branch)",
    ),
];

const SCROLLER: u32 = 1;
const LIST: u32 = 2;
/// Row nodes start here.
const FIRST_ROW: u32 = 100;
/// One device pixel at scale 1: the traces' offset tolerance.
const PX: f32 = 1.0;

/// A minimal JSON reader: the traces are ours and well formed.
mod json {
    use std::collections::BTreeMap;

    #[derive(Clone, Debug, PartialEq)]
    pub enum Value {
        Null,
        Bool(bool),
        Num(f64),
        Str(String),
        Arr(Vec<Value>),
        Obj(BTreeMap<String, Value>),
    }

    impl Value {
        pub fn get(&self, key: &str) -> Option<&Value> {
            match self {
                Value::Obj(m) => m.get(key),
                _ => None,
            }
        }

        pub fn num(&self) -> Option<f64> {
            match self {
                Value::Num(n) => Some(*n),
                _ => None,
            }
        }

        pub fn str(&self) -> Option<&str> {
            match self {
                Value::Str(s) => Some(s),
                _ => None,
            }
        }

        pub fn arr(&self) -> &[Value] {
            match self {
                Value::Arr(a) => a,
                _ => &[],
            }
        }

        pub fn bool(&self) -> Option<bool> {
            match self {
                Value::Bool(b) => Some(*b),
                _ => None,
            }
        }
    }

    pub fn parse(s: &str) -> Result<Value, String> {
        let mut p = Parser {
            b: s.as_bytes(),
            at: 0,
        };
        let v = p.value()?;
        p.ws();
        if p.at != p.b.len() {
            return Err(format!("trailing bytes at {}", p.at));
        }
        Ok(v)
    }

    struct Parser<'a> {
        b: &'a [u8],
        at: usize,
    }

    impl Parser<'_> {
        fn ws(&mut self) {
            while self.b.get(self.at).is_some_and(u8::is_ascii_whitespace) {
                self.at += 1;
            }
        }

        fn eat(&mut self, c: u8) -> Result<(), String> {
            self.ws();
            if self.b.get(self.at) != Some(&c) {
                return Err(format!("expected '{}' at {}", c as char, self.at));
            }
            self.at += 1;
            Ok(())
        }

        fn value(&mut self) -> Result<Value, String> {
            self.ws();
            match self.b.get(self.at) {
                Some(b'{') => {
                    self.at += 1;
                    let mut m = BTreeMap::new();
                    self.ws();
                    if self.b.get(self.at) == Some(&b'}') {
                        self.at += 1;
                        return Ok(Value::Obj(m));
                    }
                    loop {
                        self.ws();
                        let Value::Str(k) = self.value()? else {
                            return Err(format!("a key at {}", self.at));
                        };
                        self.eat(b':')?;
                        m.insert(k, self.value()?);
                        self.ws();
                        match self.b.get(self.at) {
                            Some(b',') => self.at += 1,
                            Some(b'}') => {
                                self.at += 1;
                                return Ok(Value::Obj(m));
                            }
                            _ => return Err(format!("',' or '}}' at {}", self.at)),
                        }
                    }
                }
                Some(b'[') => {
                    self.at += 1;
                    let mut a = Vec::new();
                    self.ws();
                    if self.b.get(self.at) == Some(&b']') {
                        self.at += 1;
                        return Ok(Value::Arr(a));
                    }
                    loop {
                        a.push(self.value()?);
                        self.ws();
                        match self.b.get(self.at) {
                            Some(b',') => self.at += 1,
                            Some(b']') => {
                                self.at += 1;
                                return Ok(Value::Arr(a));
                            }
                            _ => return Err(format!("',' or ']' at {}", self.at)),
                        }
                    }
                }
                Some(b'"') => {
                    self.at += 1;
                    let mut s = String::new();
                    loop {
                        let start = self.at;
                        while !matches!(self.b.get(self.at), Some(b'"' | b'\\') | None) {
                            self.at += 1;
                        }
                        s.push_str(std::str::from_utf8(&self.b[start..self.at]).unwrap());
                        match self.b.get(self.at) {
                            Some(b'"') => {
                                self.at += 1;
                                return Ok(Value::Str(s));
                            }
                            Some(b'\\') => {
                                let c = *self.b.get(self.at + 1).ok_or("escape")?;
                                self.at += 2;
                                s.push(match c {
                                    b'n' => '\n',
                                    b't' => '\t',
                                    b'u' => {
                                        let hex =
                                            std::str::from_utf8(&self.b[self.at..self.at + 4])
                                                .map_err(|e| e.to_string())?;
                                        self.at += 4;
                                        let c = u32::from_str_radix(hex, 16)
                                            .map_err(|e| e.to_string())?;
                                        char::from_u32(c).ok_or("a surrogate escape")?
                                    }
                                    c => c as char,
                                });
                            }
                            _ => return Err("an unterminated string".into()),
                        }
                    }
                }
                Some(b't') if self.b[self.at..].starts_with(b"true") => {
                    self.at += 4;
                    Ok(Value::Bool(true))
                }
                Some(b'f') if self.b[self.at..].starts_with(b"false") => {
                    self.at += 5;
                    Ok(Value::Bool(false))
                }
                Some(b'n') if self.b[self.at..].starts_with(b"null") => {
                    self.at += 4;
                    Ok(Value::Null)
                }
                Some(_) => {
                    let start = self.at;
                    while self
                        .b
                        .get(self.at)
                        .is_some_and(|c| c.is_ascii_digit() || b"+-.eE".contains(c))
                    {
                        self.at += 1;
                    }
                    let s = std::str::from_utf8(&self.b[start..self.at]).unwrap();
                    s.parse()
                        .map(Value::Num)
                        .map_err(|_| format!("a value at {start}"))
                }
                None => Err("unexpected end".into()),
            }
        }
    }
}

use json::Value;

/// One item of the trace's table.
#[derive(Clone, Debug)]
struct Row {
    key: String,
    version: u32,
    loaded: bool,
    failed: bool,
    /// A numeric estimate, or (template, text length).
    estimate: Result<f32, (u16, u32)>,
    /// The height its row lays out at.
    height: f32,
}

/// The rows of `blocks`, `{i}` counting from `first`.
fn rows(blocks: &[Value], first: usize) -> Vec<Row> {
    let mut out = Vec::new();
    for b in blocks {
        let n = b.get("count").and_then(Value::num).unwrap() as usize;
        let key = b.get("key").and_then(Value::str).unwrap();
        let estimate = match b.get("estimate") {
            None => Ok(48.0),
            Some(Value::Num(e)) => Ok(*e as f32),
            Some(e) => Err((
                e.get("template").and_then(Value::num).unwrap() as u16,
                e.get("textLength").and_then(Value::num).unwrap_or(0.0) as u32,
            )),
        };
        let height = match (b.get("height").and_then(Value::num), estimate) {
            (Some(h), _) => h as f32,
            (None, Ok(e)) => e,
            (None, Err(_)) => panic!("a templated block without a height"),
        };
        for _ in 0..n {
            let i = first + out.len();
            out.push(Row {
                key: key.replace("{i}", &i.to_string()),
                version: b.get("version").and_then(Value::num).unwrap_or(0.0) as u32,
                loaded: b.get("loaded").and_then(Value::bool).unwrap_or(true),
                failed: b.get("failed").and_then(Value::bool).unwrap_or(false),
                estimate,
                height,
            });
        }
    }
    out
}

fn template(t: &Value) -> Template {
    let f = |k: &str| t.get(k).and_then(Value::num).unwrap() as f32;
    match t.get("kind").and_then(Value::str).unwrap() {
        "fixed" => Template::Fixed(f("size")),
        "widths" => {
            // Native takes bands by increasing width, as the bridge
            // sends them.
            let mut bands: Vec<(f32, f32)> = (t.get("bands").unwrap().arr().iter())
                .map(|b| {
                    (
                        b.arr()[0].num().unwrap() as f32,
                        b.arr()[1].num().unwrap() as f32,
                    )
                })
                .collect();
            bands.sort_by(|a, b| a.0.total_cmp(&b.0));
            Template::Widths(bands)
        }
        "text" => Template::Text {
            base: f("base"),
            inset: f("inset"),
            font_size: f("fontSize"),
            line_height: f("lineHeight"),
            char_width: t.get("charWidth").and_then(Value::num).unwrap_or(0.5) as f32,
        },
        k => panic!("template kind {k}"),
    }
}

/// Plays the bridge and React for one list.
struct Driver {
    ui: Ui,
    view: Size,
    table: Vec<Row>,
    /// Item identity by key, and key by identity.
    ids: HashMap<String, u32>,
    keys: HashMap<u32, String>,
    /// Mounted rows: identity -> (node, version, height).
    rows: BTreeMap<u32, (u32, u32, f32)>,
    free: Vec<u32>,
    next_node: u32,
    revision: u32,
    seq: u64,
    request: u32,
    /// The current step's `updateItems` loads (first, last), and
    /// unloads (the ranges of each call that had any), in order.
    loads: Vec<(u32, u32)>,
    unloads: Vec<Vec<(u32, u32)>>,
}

impl Driver {
    fn id(&mut self, key: &str) -> u32 {
        if let Some(&id) = self.ids.get(key) {
            return id;
        }
        let id = self.ids.len() as u32 + 1;
        self.ids.insert(key.to_string(), id);
        self.keys.insert(id, key.to_string());
        id
    }

    fn item(&mut self, r: &Row) -> Item {
        let id = self.id(&r.key);
        let flags =
            if r.loaded { Item::LOADED } else { 0 } | if r.failed { Item::FAILED } else { 0 };
        match r.estimate {
            Ok(e) => Item {
                flags: flags | Item::NUMERIC,
                ..Item::sized(id, r.version, e)
            },
            Err((template, len)) => Item {
                id,
                version: r.version,
                template,
                flags,
                arg: len,
            },
        }
    }

    fn txn(&mut self) -> Transaction<'static> {
        self.seq += 1;
        Transaction::new(self.seq)
    }

    fn mount(t: &Value) -> Driver {
        let vp = t.get("viewport").unwrap();
        let view = Size::new(
            vp.get("width").and_then(Value::num).unwrap() as f32,
            vp.get("height").and_then(Value::num).unwrap() as f32,
        );
        let config = t.get("config").unwrap();
        let num = |k: &str| config.get(k).and_then(Value::num).map(|v| v as f32);
        let mut d = Driver {
            ui: Ui::new(1.0),
            view,
            table: rows(t.get("items").unwrap().arr(), 0),
            ids: HashMap::new(),
            keys: HashMap::new(),
            rows: BTreeMap::new(),
            free: Vec::new(),
            next_node: FIRST_ROW,
            revision: 0,
            seq: 0,
            request: 0,
            loads: Vec::new(),
            unloads: Vec::new(),
        };
        let templates: Vec<Template> = t
            .get("templates")
            .unwrap()
            .arr()
            .iter()
            .map(template)
            .collect();
        let end = config.get("anchor").and_then(Value::str) == Some("end");
        let policy = ListPolicy {
            mode: if end {
                Anchor::StickToEnd
            } else {
                Anchor::KeepVisible
            },
            focus: config.get("anchorPolicy").and_then(Value::str) == Some("focus"),
            end_threshold: num("endThreshold").unwrap_or(80.0),
            start_inset: num("startInset").unwrap_or(0.0),
            padding_end: num("paddingEnd").unwrap_or(0.0),
        };
        let items: Vec<Item> = (0..d.table.len())
            .map(|i| {
                let r = d.table[i].clone();
                d.item(&r)
            })
            .collect();
        let mut tx = d.txn();
        let scroller = taffy::Style {
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            overflow: taffy::Point {
                x: taffy::Overflow::Visible,
                y: taffy::Overflow::Scroll,
            },
            ..craie_ui::host::default_style().to_taffy()
        };
        let list = taffy::Style {
            flex_shrink: 0.0,
            ..craie_ui::host::default_style().to_taffy()
        };
        tx.create(SCROLLER, NodeKind::View)
            .layout(SCROLLER, &scroller)
            .list_policy(SCROLLER, policy)
            .append(NIL, SCROLLER);
        tx.create(LIST, NodeKind::List)
            .layout(LIST, &list)
            .list_config2(
                LIST,
                num("overscan").unwrap_or(600.0),
                num("lookahead").unwrap_or(-1.0),
                num("retain").unwrap_or(3.0),
                48.0,
                0,
                &templates,
            )
            .list_patch(LIST, 0, 1, &[ListOp::splice(0, 0, &items)])
            .interaction(LIST, mask::UPDATE_ITEMS | mask::VISIBLE_CHANGE, false)
            .append(SCROLLER, LIST);
        d.revision = 1;
        d.ui.apply_txn(&tx).expect("the list mounts");
        d
    }

    /// Records the list's `updateItems` loads, then mounts the rows of
    /// the latest viewport it reported (and its pinned rows), as one
    /// commit, re-rendering mounted rows whose item changed. Returns
    /// whether it committed.
    fn pump(&mut self) -> bool {
        let mut want = None;
        for e in self.ui.take_events() {
            if e.node != LIST {
                continue;
            }
            if e.kind == out_kind::CALL && e.key & 0xFF == list_slot::UPDATE_ITEMS {
                let u = |at: usize| u32::from_le_bytes(e.payload[at..at + 4].try_into().unwrap());
                let mut at = 1;
                let mut range = || {
                    at += 8;
                    (u(at - 8), u(at - 4))
                };
                if e.payload[0] & 1 != 0 {
                    self.loads.push(range());
                }
                let unload: Vec<(u32, u32)> = (1..3)
                    .filter(|b| e.payload[0] & (1 << b) != 0)
                    .map(|_| range())
                    .collect();
                if !unload.is_empty() {
                    self.unloads.push(unload);
                }
            }
            if e.kind == out_kind::LIST_VIEWPORT && e.key == 0 {
                want = Some(e.payload);
            }
        }
        let Some(v) = want else {
            return false;
        };
        let u = |at: usize| u32::from_le_bytes(v[at..at + 4].try_into().unwrap()) as usize;
        let mut wanted: Vec<usize> = (u(0)..u(4).min(self.table.len())).collect();
        let pinned = u16::from_le_bytes([v[41], v[42]]) as usize;
        for k in 0..pinned {
            let id = u(43 + 4 * k) as u32;
            if let Some(i) = self
                .table
                .iter()
                .position(|r| self.ids.get(&r.key) == Some(&id))
                && !wanted.contains(&i)
            {
                wanted.push(i);
            }
        }
        let rows: Vec<Row> = (wanted.into_iter())
            .filter_map(|i| self.table.get(i).cloned())
            .collect();
        let wanted: Vec<(u32, u32, f32)> = (rows.iter())
            .map(|r| (self.id(&r.key), r.version, r.height))
            .collect();
        let mut t = self.txn();
        for (id, row) in std::mem::take(&mut self.rows) {
            if wanted.iter().any(|w| w.0 == id) {
                self.rows.insert(id, row);
            } else {
                t.remove(row.0);
                self.free.push(row.0);
            }
        }
        // Rows mount, or render again with their item's version and
        // height.
        for (id, version, height) in wanted {
            let node = match self.rows.get(&id) {
                Some(&(_, v, h)) if v == version && h == height => continue,
                Some(&(node, _, _)) => node,
                None => {
                    let node = self.free.pop().unwrap_or_else(|| {
                        self.next_node += 1;
                        self.next_node
                    });
                    t.create(node, NodeKind::View)
                        .interaction(node, 0, true)
                        .append(LIST, node);
                    node
                }
            };
            self.rows.insert(id, (node, version, height));
            t.layout(node, &row_style(height))
                .list_row(node, LIST, id, version);
        }
        if t.mutations.is_empty() {
            return false;
        }
        self.ui.apply_txn(&t).expect("rows apply");
        true
    }

    /// Renders until the rendered rows are those the list asks for.
    fn settle(&mut self) {
        self.ui.render(self.view);
        for _ in 0..16 {
            if !self.pump() {
                return;
            }
            self.ui.render(self.view);
        }
        // Re-rendering the same rows changes nothing: a last frame.
        self.ui.render(self.view);
    }

    fn command(&mut self, jump: Jump) {
        self.request += 1;
        let mut t = self.txn();
        t.list_command(LIST, self.revision, self.request, jump);
        self.ui.apply_txn(&t).expect("the command applies");
    }

    /// One commit's batch: ops against the table, as one patch.
    fn changes(&mut self, ops: &[Value]) {
        let mut patch = Vec::new();
        for op in ops {
            let n = |k: &str| op.get(k).and_then(Value::num).unwrap() as usize;
            match op.get("kind").and_then(Value::str).unwrap() {
                "splice" => {
                    let (at, remove) = (n("at"), n("remove"));
                    let new = rows(op.get("items").unwrap().arr(), at);
                    let items: Vec<Item> = new.iter().map(|r| self.item(r)).collect();
                    self.table.splice(at..at + remove, new);
                    patch.push(ListOp::splice(at as u32, remove as u32, &items));
                }
                "move" => {
                    let (from, count, to) = (n("from"), n("count"), n("to"));
                    let moved: Vec<Row> = self.table.drain(from..from + count).collect();
                    self.table.splice(to..to, moved);
                    patch.push(ListOp::Move {
                        from: from as u32,
                        count: count as u32,
                        to: to as u32,
                    });
                }
                "update" => {
                    let at = n("at");
                    let new = rows(op.get("items").unwrap().arr(), at);
                    let items: Vec<Item> = new.iter().map(|r| self.item(r)).collect();
                    let k = new.len();
                    self.table.splice(at..at + k, new);
                    patch.push(ListOp::update(at as u32, &items));
                }
                k => panic!("op {k}"),
            }
        }
        let mut t = self.txn();
        t.list_patch(LIST, self.revision, self.revision + 1, &patch);
        self.revision += 1;
        // Mounted rows of changed items render their new version in the
        // same commit, as React does.
        for (id, (node, version, height)) in self.rows.clone() {
            let Some(r) = self.table.iter().find(|r| self.ids[&r.key] == id) else {
                continue;
            };
            if r.version != version || r.height != height {
                t.layout(node, &row_style(r.height))
                    .list_row(node, LIST, id, r.version);
                self.rows.insert(id, (node, r.version, r.height));
            }
        }
        self.ui.apply_txn(&t).expect("the batch applies");
    }

    fn measure(&mut self, key: &str, height: f32) {
        let i = self
            .table
            .iter()
            .position(|r| r.key == key)
            .expect("a measured key");
        self.table[i].height = height;
        let id = self.ids[key];
        if let Some(e) = self.rows.get_mut(&id) {
            e.2 = height;
            let node = e.0;
            let mut t = self.txn();
            t.layout(node, &row_style(height));
            self.ui.apply_txn(&t).expect("the row resizes");
        }
    }

    fn step(&mut self, s: &Value) -> Result<(), String> {
        self.loads.clear();
        self.unloads.clear();
        let num = |k: &str| s.get(k).and_then(Value::num);
        let align = || match s.get("align").and_then(Value::str) {
            Some("center") => Align::Center,
            Some("end") => Align::End,
            _ => Align::Start,
        };
        match s.get("do").and_then(Value::str).unwrap() {
            "mount" => {}
            "scroll" => {
                self.ui
                    .scroll_to(NodeId(SCROLLER), 0.0, num("offset").unwrap() as f32);
            }
            "scrollBy" => {
                let y = self.ui.scroll_offset(NodeId(SCROLLER))[1];
                self.ui
                    .scroll_to(NodeId(SCROLLER), 0.0, y + num("delta").unwrap() as f32);
            }
            "scrollToIndex" => self.command(Jump::Index(num("index").unwrap() as u32, align())),
            "scrollToKey" => {
                let key = s.get("key").and_then(Value::str).unwrap();
                let id = self.ids.get(key).copied().unwrap_or(NIL - 1);
                self.command(Jump::Item(id, align()));
            }
            "scrollToEnd" => self.command(Jump::End),
            "scrollToOffset" => self.command(Jump::Offset(num("offset").unwrap())),
            "changes" => self.changes(s.get("changes").unwrap().arr()),
            "measure" => self.measure(
                s.get("key").and_then(Value::str).unwrap(),
                num("height").unwrap() as f32,
            ),
            "focus" | "blur" => {
                let key = s.get("key").and_then(Value::str).unwrap();
                let Some(&(node, _, _)) = self.ids.get(key).and_then(|id| self.rows.get(id)) else {
                    return Err(format!("{key} isn't mounted"));
                };
                let mut t = self.txn();
                let cmd = if s.get("do").and_then(Value::str) == Some("focus") {
                    Command::Focus
                } else {
                    Command::Blur
                };
                t.command(node, cmd);
                self.ui.apply_txn(&t).expect("the focus applies");
            }
            "resize" => {
                self.view = Size::new(num("width").unwrap() as f32, num("height").unwrap() as f32);
            }
            other => return Err(format!("step '{other}' isn't driven yet")),
        }
        self.settle();
        Ok(())
    }

    /// Checks a step's expectations.
    fn check(&self, e: &Value) -> Result<(), String> {
        let v = self
            .ui
            .list_viewport(NodeId(LIST))
            .ok_or("no list viewport")?;
        let mut errs = Vec::new();
        let pair = |v: &Value| {
            (
                v.arr()[0].num().unwrap() as i64,
                v.arr()[1].num().unwrap() as i64,
            )
        };
        if let Some(want) = e.get("visible") {
            let got = (v.visible.start as i64, v.visible.end as i64 - 1);
            let want = pair(want);
            let empty = |r: (i64, i64)| r.1 < r.0;
            if got != want && !(empty(got) && empty(want)) {
                errs.push(format!("visible {got:?}, expected {want:?}"));
            }
        }
        if let Some(want) = e.get("anchor") {
            let key = want.get("key").and_then(Value::str).unwrap();
            let offset = want.get("offset").and_then(Value::num).unwrap() as f32;
            match v.anchor {
                Some((id, _, o)) if self.keys.get(&id).map(String::as_str) == Some(key) => {
                    if (o - offset).abs() > PX {
                        errs.push(format!("anchor {key} at {o}, expected {offset}"));
                    }
                }
                got => {
                    let got = got.map(|(id, _, o)| (self.keys.get(&id).cloned(), o));
                    errs.push(format!("anchor {got:?}, expected {key} at {offset}"));
                }
            }
        }
        if let Some(want) = e.get("offset").and_then(Value::num)
            && (v.offset - want as f32).abs() > PX
        {
            errs.push(format!("offset {}, expected {want}", v.offset));
        }
        if let Some(want) = e.get("atEnd").and_then(Value::bool)
            && v.at_end != want
        {
            errs.push(format!("atEnd {}, expected {want}", v.at_end));
        }
        if let Some(want) = e.get("following").and_then(Value::bool)
            && v.following != want
        {
            errs.push(format!("following {}, expected {want}", v.following));
        }
        if let Some(want) = e.get("pinnedKeys") {
            let mut want: Vec<&str> = want.arr().iter().filter_map(Value::str).collect();
            let mut got: Vec<&str> = (v.pinned.iter())
                .filter_map(|id| self.keys.get(id).map(String::as_str))
                .collect();
            want.sort_unstable();
            got.sort_unstable();
            if got != want {
                errs.push(format!("pinnedKeys {got:?}, expected {want:?}"));
            }
        }
        if let Some(want) = e.get("mounted") {
            let (a, b) = pair(want.get("covers").unwrap());
            let mounted: Vec<i64> = (a..=b)
                .filter(|&i| {
                    let key = &self.table[i as usize].key;
                    self.rows.contains_key(&self.ids[key])
                })
                .collect();
            if mounted.len() as i64 != b - a + 1 {
                errs.push(format!("mounted doesn't cover [{a}, {b}]"));
            }
        }
        if let Some(want) = e.get("load") {
            let got = self.loads.last().copied();
            let ok = match want {
                Value::Null => got.is_none(),
                Value::Arr(_) => {
                    let (a, b) = pair(want);
                    got == Some((a as u32, b as u32))
                }
                w => {
                    let (a, b) = pair(w.get("covers").unwrap());
                    got.is_some_and(|(x, y)| x as i64 <= a && b <= y as i64)
                }
            };
            if !ok {
                errs.push(format!(
                    "load {:?} (this step: {:?}), expected {want:?}",
                    got, self.loads
                ));
            }
        }
        if let Some(want) = e.get("unload") {
            let want: Vec<(u32, u32)> = (want.arr().iter())
                .map(|r| {
                    let (a, b) = pair(r);
                    (a as u32, b as u32)
                })
                .collect();
            let got = self.unloads.last().cloned().unwrap_or_default();
            if got != want || self.unloads.len() > 1 {
                errs.push(format!(
                    "unload {:?} (this step: {:?}), expected {want:?}",
                    got, self.unloads
                ));
            }
        }
        if let Some(want) = e.get("held") {
            let got = (!v.held.is_empty()).then(|| (v.held.start as i64, v.held.end as i64 - 1));
            let want = (*want != Value::Null).then(|| pair(want));
            if got != want {
                errs.push(format!("held {got:?}, expected {want:?}"));
            }
        }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs.join("; "))
        }
    }
}

fn row_style(height: f32) -> taffy::Style {
    taffy::Style {
        size: taffy::Size {
            width: taffy::Dimension::auto(),
            height: taffy::Dimension::length(height),
        },
        flex_shrink: 0.0,
        ..craie_ui::host::default_style().to_taffy()
    }
}

/// Plays trace `t`: the first failing step's error, if any.
fn play(t: &Value) -> Result<(), String> {
    let mut d = Driver::mount(t);
    for (n, s) in t.get("steps").unwrap().arr().iter().enumerate() {
        let what = s.get("do").and_then(Value::str).unwrap_or("?");
        d.step(s).map_err(|e| format!("step {n} ({what}): {e}"))?;
        if let Some(e) = s.get("expect") {
            d.check(e).map_err(|e| format!("step {n} ({what}): {e}"))?;
        }
    }
    Ok(())
}

#[test]
fn shared_list_traces() {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packages/bridge/traces/lists"
    );
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .expect("the trace directory")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    assert!(files.len() >= 31, "{} traces", files.len());
    let mut wrong = Vec::new();
    let mut passed = Vec::new();
    for f in &files {
        let t = json::parse(&std::fs::read_to_string(f).unwrap())
            .unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        let id = t.get("id").and_then(Value::str).unwrap().to_string();
        let pending = PENDING.iter().find(|p| p.0 == id);
        match (play(&t), pending) {
            (Ok(()), None) => passed.push(id),
            (Ok(()), Some(_)) => wrong.push(format!("{id} passes: take it off PENDING")),
            (Err(e), None) => wrong.push(format!(
                "{id} ({}): {e}",
                t.get("name").and_then(Value::str).unwrap()
            )),
            (Err(e), Some(_)) => eprintln!("pending {id}: {e}"),
        }
    }
    for (id, _) in PENDING {
        let known = files.iter().any(|f| {
            f.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with(&format!("{id}-"))
        });
        assert!(known, "PENDING names {id}, which has no file");
    }
    assert!(wrong.is_empty(), "passed {passed:?}\n{}", wrong.join("\n"));
}
