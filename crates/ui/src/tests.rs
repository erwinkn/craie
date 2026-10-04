//! Facade-level tests: execution, layout, paint, dispatch, and the
//! accessibility projection, driven through transactions.

use crate::events::{Key, KeyInput, Mods, mask, out_kind};
use crate::geom::Size;
use crate::host::NodeId;
use crate::mutation::{Command, Mutation, NodeKind, Role, TextSpan, Transaction};
use crate::scene::{Resolved, Scene};
use crate::surface;
use crate::ui::Ui;
use crate::wire::{self, WireError};

const NIL: u32 = u32::MAX;

/// A named edit that must make a transaction fail.
type BadOp = (&'static str, fn(&mut Transaction));

/// Every drawn primitive in draw order, in device space.
fn resolved(scene: &Scene) -> Vec<Resolved> {
    scene.resolve(&|r| r.0 as u64)
}

/// Glyphs in the drawn scene.
fn glyphs(scene: &Scene) -> usize {
    resolved(scene).iter().filter(|p| p.kind == 1).count()
}

const WORDS: &str = "word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word ";

fn txn() -> Vec<u8> {
    let mut t = Transaction::new(1);
    let mut s = taffy::Style::default();
    s.display = taffy::Display::Flex;
    // Column container: children get the container width as definite
    // cross-axis space, which is what wraps text to the viewport.
    s.flex_direction = taffy::FlexDirection::Column;
    s.size = taffy::Size {
        width: taffy::Dimension::percent(1.0),
        height: taffy::Dimension::percent(1.0),
    };
    let st1 = t.style(&s);
    t.create(0, NodeKind::View);
    t.push(Mutation::Layout { id: 0, style: st1 });
    t.place(NIL, 0, NIL);
    t.create(1, NodeKind::Text);
    t.text(1, WORDS, 20.0, 0xFFFF_FFFF);
    t.place(0, 1, NIL);
    wire::encode(&t)
}

/// The resize path: no mutation arrives, `invalidate_layout` alone
/// must cause text to rewrap to the new available width.
#[test]
fn resize_reflows_text() {
    let mut ui = Ui::new(1.0);
    ui.apply(&txn()).unwrap();
    ui.render(Size::new(1600.0, 800.0));
    let wide = ui.text_layout(NodeId(1)).unwrap().lines.len();

    ui.invalidate_layout();
    ui.render(Size::new(300.0, 800.0));
    let narrow = ui.text_layout(NodeId(1)).unwrap().lines.len();

    assert!(
        narrow > wide,
        "narrower viewport must wrap to more lines: {wide} -> {narrow}"
    );
}

/// `display: none` subtrees emit nothing but don't truncate siblings.
#[test]
fn display_none_keeps_siblings() {
    let mut t = Transaction::new(1);
    let mut gone = taffy::Style::default();
    gone.display = taffy::Display::None;
    let mut s = taffy::Style::default();
    s.display = taffy::Display::Flex;
    s.size = taffy::Size {
        width: taffy::Dimension::percent(1.0),
        height: taffy::Dimension::percent(1.0),
    };
    let st1 = t.style(&s);
    t.create(0, NodeKind::View);
    t.push(Mutation::Layout { id: 0, style: st1 });
    t.place(NIL, 0, NIL);
    // The middle node is hidden; "after" must still paint — a hidden
    // node must not truncate its sibling chain.
    for (id, text) in [(2u32, "before"), (3, "hidden"), (4, "after")] {
        t.create(id, NodeKind::Text);
        t.text(id, text, 20.0, 0xFFFF_FFFF);
        if id == 3 {
            t.layout(id, &gone);
        }
        t.place(0, id, NIL);
    }
    t.seq = 1;
    let buf = wire::encode(&t);
    let mut ui = Ui::new(1.0);
    ui.apply(&buf).unwrap();
    let hidden_count = glyphs(ui.render(Size::new(800.0, 600.0)));
    assert!(hidden_count > 0);

    let mut t = Transaction::new(1);
    t.layout(3, &taffy::Style::default());
    t.seq = 2;
    let buf = wire::encode(&t);
    ui.apply(&buf).unwrap();
    let shown_count = glyphs(ui.render(Size::new(800.0, 600.0)));
    assert!(
        shown_count > hidden_count,
        "unhiding node 3 must add its glyphs: {hidden_count} -> {shown_count}"
    );
}

/// Nested padding+border: a child's accumulated origin must include
/// each ancestor's insets exactly once (the layout location is
/// relative to the parent's content box).
#[test]
fn nested_insets_accumulate_once() {
    let mut t = Transaction::new(1);
    let mut outer = taffy::Style::default();
    outer.display = taffy::Display::Flex;
    outer.padding = taffy::Rect {
        left: taffy::LengthPercentage::length(10.0),
        right: taffy::LengthPercentage::length(0.0),
        top: taffy::LengthPercentage::length(20.0),
        bottom: taffy::LengthPercentage::length(0.0),
    };
    outer.border = taffy::Rect {
        left: taffy::LengthPercentage::length(3.0),
        right: taffy::LengthPercentage::length(0.0),
        top: taffy::LengthPercentage::length(4.0),
        bottom: taffy::LengthPercentage::length(0.0),
    };
    let st1 = t.style(&outer);
    let mut inner = taffy::Style::default();
    inner.display = taffy::Display::Flex;
    inner.padding = taffy::Rect {
        left: taffy::LengthPercentage::length(5.0),
        right: taffy::LengthPercentage::length(0.0),
        top: taffy::LengthPercentage::length(7.0),
        bottom: taffy::LengthPercentage::length(0.0),
    };
    let st2 = t.style(&inner);
    t.create(0, NodeKind::View);
    t.push(Mutation::Layout { id: 0, style: st1 });
    t.place(NIL, 0, NIL);
    t.create(1, NodeKind::View);
    t.push(Mutation::Layout { id: 1, style: st2 });
    t.place(0, 1, NIL);
    t.create(2, NodeKind::View);
    t.fill(2, 0xFF00_00FF);
    t.place(1, 2, NIL);
    t.seq = 1;
    let buf = wire::encode(&t);

    let mut ui = Ui::new(1.0);
    ui.apply(&buf).unwrap();
    ui.render(Size::new(800.0, 600.0));

    // Inner's border box sits at the outer's content origin (13, 24).
    // The leaf's border box adds inner's content offset (5, 7).
    let leaf = ui.layouts.data(NodeId(2));
    assert_eq!(leaf.rect.origin.x, 5.0);
    assert_eq!(leaf.rect.origin.y, 7.0);
    let inner_data = ui.layouts.data(NodeId(1));
    assert_eq!(inner_data.rect.origin.x, 13.0);
    assert_eq!(inner_data.rect.origin.y, 24.0);

    // Paint-time accumulated origin: the leaf quad emits at the
    // accumulated content origin (13+5, 24+7) = (18, 31) — insets
    // counted exactly once.
    let quad = resolved(ui.scene())
        .into_iter()
        .find(|p| p.kind == 0)
        .expect("leaf quad emitted");
    assert_eq!((quad.bounds.origin.x, quad.bounds.origin.y), (18.0, 31.0));
}

/// `display: none` must remove the subtree from layout AND paint —
/// the flag path and the style path are equivalent.
#[test]
fn display_none_skips_subtree() {
    let mut t = Transaction::new(1);
    let mut s = taffy::Style::default();
    s.display = taffy::Display::Flex;
    s.flex_direction = taffy::FlexDirection::Column;
    let st1 = t.style(&s);
    let mut gone = taffy::Style::default();
    gone.display = taffy::Display::None;
    let st2 = t.style(&gone);

    t.create(0, NodeKind::View);
    t.push(Mutation::Layout { id: 0, style: st1 });
    t.place(NIL, 0, NIL);
    // Hidden-by-style container with a text child.
    t.create(1, NodeKind::View);
    t.push(Mutation::Layout { id: 1, style: st2 });
    t.place(0, 1, NIL);
    t.create(2, NodeKind::Text);
    t.text(2, "invisible", 14.0, 0xFFFF_FFFF);
    t.place(1, 2, NIL);
    // Sibling still paints.
    t.create(3, NodeKind::Text);
    t.text(3, "visible", 14.0, 0xFFFF_FFFF);
    t.place(0, 3, NIL);
    t.seq = 1;
    let buf = wire::encode(&t);

    let mut ui = Ui::new(1.0);
    ui.apply(&buf).unwrap();
    let scene = ui.render(Size::new(800.0, 600.0));
    let n = glyphs(scene);
    assert!(n > 0);
    // "invisible" (9 chars) must not emit; "visible" (7) does.
    assert!(n < 10, "{n} glyphs");
}

// ------------------------------------------------------ dispatch

use crate::events::{Button, Event};

/// A scrollable 100px container with a 300px column of children.
/// Returns (ui, container id, child id).
fn scroll_ui() -> (Ui, NodeId, NodeId) {
    let mut t = Transaction::new(1);
    let mut s = taffy::Style::default();
    s.display = taffy::Display::Flex;
    s.overflow = taffy::Point {
        x: taffy::Overflow::Scroll,
        y: taffy::Overflow::Scroll,
    };
    s.size = taffy::Size {
        width: taffy::Dimension::length(100.0),
        height: taffy::Dimension::length(100.0),
    };
    let st1 = t.style(&s);
    let mut child = taffy::Style::default();
    child.size = taffy::Size {
        width: taffy::Dimension::length(100.0),
        height: taffy::Dimension::length(300.0),
    };
    let st2 = t.style(&child);
    t.create(0, NodeKind::View);
    t.push(Mutation::Layout { id: 0, style: st1 });
    t.place(NIL, 0, NIL);
    t.create(1, NodeKind::View);
    t.push(Mutation::Layout { id: 1, style: st2 });
    t.place(0, 1, NIL);
    t.seq = 1;
    let buf = wire::encode(&t);
    let mut ui = Ui::new(1.0);
    ui.apply(&buf).unwrap();
    ui.render(Size::new(800.0, 600.0));
    (ui, NodeId(0), NodeId(1))
}

/// A wheel event scrolls the nearest scrollable ancestor and emits a
/// SCROLL event when the node subscribed.
#[test]
fn wheel_scrolls_and_reports() {
    let (mut ui, container, child) = scroll_ui();
    // Subscribe to scroll events on the container.
    let mut t = Transaction::new(1);
    t.interaction(0, mask::SCROLL, false);
    t.seq = 2;
    let buf = wire::encode(&t);
    ui.apply(&buf).unwrap();
    ui.render(Size::new(800.0, 600.0));

    ui.dispatch(&Event::Wheel {
        x: 10.0,
        y: 10.0,
        dx: 0.0,
        dy: 50.0,
    });
    assert_eq!(ui.scroll_offset(container), [0.0, 50.0]);

    let events = ui.take_events();
    let scroll = events
        .iter()
        .find(|e| e.kind == out_kind::SCROLL)
        .expect("scroll event emitted");
    assert_eq!(scroll.node, container.0);
    assert_eq!((scroll.a, scroll.b), (0.0, 50.0));

    // Scrolled: the hit under (10,10) is still inside the child (the
    // child is 300 tall) but the child must now answer at a scrolled
    // position — a hit at y=250 hits nothing outside the container's
    // clip.
    assert_eq!(ui.hit_test(10.0, 250.0), None);
    assert_eq!(ui.hit_test(10.0, 10.0), Some(child));

    // Clamp at the extent: 300 - 100 = 200.
    ui.dispatch(&Event::Wheel {
        x: 10.0,
        y: 10.0,
        dx: 0.0,
        dy: 1000.0,
    });
    assert_eq!(ui.scroll_offset(container), [0.0, 200.0]);
}

/// Enter and leave go to each side's chain below the common ancestor,
/// deepest first; a subtree that leaves the tree under the pointer
/// hands the hover to its parent, so the ancestors that stay hovered
/// get no second enter.
#[test]
fn pointer_enter_leave_sequences() {
    // Roots A at (0, 0) and D at (0, 300); in A, B | C side by side;
    // in B, B1 | B2, and E in B2's corner; C1 in C's corner.
    let (a, b, b1, b2, e, c, c1, d) = (1, 2, 3, 4, 5, 6, 7, 8);
    let at = |x: f32, y: f32, w: f32, h: f32| taffy::Style {
        position: taffy::Position::Absolute,
        inset: taffy::Rect {
            left: taffy::LengthPercentageAuto::length(x),
            top: taffy::LengthPercentageAuto::length(y),
            ..taffy::Rect::auto()
        },
        size: taffy::Size {
            width: taffy::Dimension::length(w),
            height: taffy::Dimension::length(h),
        },
        ..taffy::Style::default()
    };
    let mut t = Transaction::new(1);
    for (id, parent, style) in [
        (a, NIL, at(0.0, 0.0, 400.0, 200.0)),
        (b, a, at(0.0, 0.0, 200.0, 200.0)),
        (b1, b, at(0.0, 0.0, 100.0, 100.0)),
        (b2, b, at(100.0, 0.0, 100.0, 100.0)),
        (e, b2, at(0.0, 0.0, 50.0, 50.0)),
        (c, a, at(200.0, 0.0, 200.0, 200.0)),
        (c1, c, at(0.0, 0.0, 100.0, 100.0)),
        (d, NIL, at(0.0, 0.0, 100.0, 100.0)),
    ] {
        t.create(id, NodeKind::View)
            .layout(id, &style)
            .interaction(id, mask::POINTER_ENTER_LEAVE, false)
            .append(parent, id);
    }
    // A root sits at the window's origin; D moves down by a transform.
    t.transform(d, craie_core::geom::Affine::translate(0.0, 300.0));
    let mut ui = Ui::new(1.0);
    ui.apply_txn(&t).unwrap();
    let view = Size::new(800.0, 600.0);
    ui.render(view);

    const IN: u8 = out_kind::POINTER_ENTER;
    const OUT: u8 = out_kind::POINTER_LEAVE;
    let to = |ui: &mut Ui, x: f32, y: f32| -> Vec<(u8, u32)> {
        ui.dispatch(&crate::events::Event::PointerMove { x, y });
        ui.take_events()
            .iter()
            .filter(|e| e.kind == IN || e.kind == OUT)
            .map(|e| (e.kind, e.node))
            .collect()
    };
    let edit = |ui: &mut Ui, f: &dyn Fn(&mut Transaction)| {
        let mut t = Transaction::new(2);
        f(&mut t);
        ui.apply_txn(&t).unwrap();
        ui.render(view);
    };

    assert_eq!(to(&mut ui, 50.0, 50.0), [(IN, b1), (IN, b), (IN, a)]);
    // Siblings.
    assert_eq!(to(&mut ui, 175.0, 75.0), [(OUT, b1), (IN, b2)]);
    assert_eq!(to(&mut ui, 125.0, 25.0), [(IN, e)]);
    // Cousins.
    assert_eq!(
        to(&mut ui, 250.0, 50.0),
        [(OUT, e), (OUT, b2), (OUT, b), (IN, c1), (IN, c)]
    );
    // Separate roots, both ways.
    assert_eq!(
        to(&mut ui, 50.0, 350.0),
        [(OUT, c1), (OUT, c), (OUT, a), (IN, d)]
    );
    assert_eq!(
        to(&mut ui, 125.0, 25.0),
        [(OUT, d), (IN, e), (IN, b2), (IN, b), (IN, a)]
    );
    // The hovered node's parent is removed: B and A stay hovered.
    edit(&mut ui, &|t| {
        t.remove(b2);
    });
    assert_eq!(to(&mut ui, 50.0, 150.0), []);
    assert_eq!(to(&mut ui, 50.0, 50.0), [(IN, b1)]);
    // The hovered node's grandparent is detached: A stays hovered.
    edit(&mut ui, &|t| {
        t.detach(b);
    });
    assert_eq!(to(&mut ui, 300.0, 150.0), [(IN, c)]);
    // The hovered node itself is removed.
    edit(&mut ui, &|t| {
        t.remove(c);
    });
    assert_eq!(to(&mut ui, 50.0, 150.0), []);
    assert_eq!(to(&mut ui, 50.0, 350.0), [(OUT, a), (IN, d)]);
}

/// Clipped children are not hit outside the clip rect.
#[test]
fn hit_test_respects_clip() {
    let (ui, _container, _child) = scroll_ui();
    // Child is 300 tall but the container clips at 100.
    assert_eq!(ui.hit_test(50.0, 150.0), None);
}

/// Pointer down focuses the nearest focusable ancestor; Tab cycles
/// the focus ring and emits blur/focus events.
#[test]
fn focus_click_and_tab() {
    let mut t = Transaction::new(1);
    let mut s = taffy::Style::default();
    s.size = taffy::Size {
        width: taffy::Dimension::length(50.0),
        height: taffy::Dimension::length(20.0),
    };
    let st1 = t.style(&s);
    t.create(0, NodeKind::View);
    t.push(Mutation::Layout { id: 0, style: st1 });
    t.interaction(0, mask::FOCUS, true);
    t.place(NIL, 0, NIL);
    t.create(1, NodeKind::View);
    t.push(Mutation::Layout { id: 1, style: st1 });
    t.interaction(1, mask::FOCUS, true);
    t.place(0, 1, NIL);
    t.seq = 1;
    let buf = wire::encode(&t);
    let mut ui = Ui::new(1.0);
    ui.apply(&buf).unwrap();
    ui.render(Size::new(800.0, 600.0));

    let mods = Mods::default();
    ui.dispatch(&Event::PointerDown {
        x: 10.0,
        y: 10.0,
        button: Button::Primary,
        mods,
    });
    // The click landed on child 1; its ancestor chain's first
    // focusable is the child itself.
    assert_eq!(ui.focused(), Some(NodeId(1)));
    assert!(
        ui.take_events()
            .iter()
            .any(|e| e.kind == out_kind::FOCUS && e.node == 1)
    );

    // Tab moves to the next focusable (wraps to 0, the parent is
    // earlier in document order).
    ui.dispatch(&Event::KeyDown(KeyInput {
        key: Key::Tab,
        text: None,
        char: None,
        mods,
        ..KeyInput::default()
    }));
    assert_eq!(ui.focused(), Some(NodeId(0)));
    let events = ui.take_events();
    assert!(
        events
            .iter()
            .any(|e| e.kind == out_kind::BLUR && e.node == 1)
    );
    assert!(
        events
            .iter()
            .any(|e| e.kind == out_kind::FOCUS && e.node == 0)
    );

    // Shift+Tab wraps backward.
    ui.dispatch(&Event::KeyDown(KeyInput {
        key: Key::Tab,
        text: None,
        char: None,
        mods: Mods {
            shift: true,
            ..Mods::default()
        },
        ..KeyInput::default()
    }));
    assert_eq!(ui.focused(), Some(NodeId(1)));
}

/// Typing into a focused input mutates the buffer natively and emits
/// a CHANGE event with the committed text.
#[test]
fn input_typing_changes_buffer() {
    let mut t = Transaction::new(1);
    let mut s = taffy::Style::default();
    s.size = taffy::Size {
        width: taffy::Dimension::length(200.0),
        height: taffy::Dimension::length(30.0),
    };
    let st1 = t.style(&s);
    t.create(0, NodeKind::Input);
    t.push(Mutation::Layout { id: 0, style: st1 });
    t.input_config(0, 16.0, "", false);
    t.interaction(0, mask::INPUT, true);
    t.place(NIL, 0, NIL);
    t.seq = 1;
    let buf = wire::encode(&t);
    let mut ui = Ui::new(1.0);
    ui.apply(&buf).unwrap();
    ui.render(Size::new(800.0, 600.0));

    // Click into the field, then type.
    ui.dispatch(&Event::PointerDown {
        x: 5.0,
        y: 5.0,
        button: Button::Primary,
        mods: Mods::default(),
    });
    assert_eq!(ui.focused(), Some(NodeId(0)));
    ui.dispatch(&Event::KeyDown(KeyInput {
        key: Key::Unknown,
        text: Some("h".into()),
        char: Some("h".into()),
        mods: Mods::default(),
        ..KeyInput::default()
    }));
    ui.dispatch(&Event::KeyDown(KeyInput {
        key: Key::Unknown,
        text: Some("i".into()),
        char: Some("i".into()),
        mods: Mods::default(),
        ..KeyInput::default()
    }));
    assert_eq!(ui.inputs.text(0), "hi");

    let events = ui.take_events();
    let changes: Vec<&str> = events
        .iter()
        .filter(|e| e.kind == out_kind::CHANGE)
        .map(|e| e.text.as_str())
        .collect();
    assert_eq!(changes, ["h", "hi"]);

    // Enter on single-line emits SUBMIT, not a newline.
    ui.dispatch(&Event::KeyDown(KeyInput {
        key: Key::Enter,
        text: None,
        char: None,
        mods: Mods::default(),
        ..KeyInput::default()
    }));
    assert!(
        ui.take_events()
            .iter()
            .any(|e| e.kind == out_kind::SUBMIT && e.text == "hi")
    );
}

/// A bars surface paints one quad per payload value, scaled and
/// clipped like any node content. No JS runs at paint time.
#[test]
fn surface_bars_paint_from_payload() {
    let mut t = Transaction::new(1);
    let mut s = taffy::Style::default();
    s.size = taffy::Size {
        width: taffy::Dimension::length(100.0),
        height: taffy::Dimension::length(40.0),
    };
    let st1 = t.style(&s);
    let values: Vec<u8> = [0.5f32, 1.0].iter().flat_map(|v| v.to_le_bytes()).collect();
    t.create(0, NodeKind::Surface);
    t.push(Mutation::Layout { id: 0, style: st1 });
    t.surface(0, surface::kind::BARS, [0xFF00_00FF, 0x00FF_00FF, 0, 0]);
    t.payload(0, &values);
    t.place(NIL, 0, NIL);
    let buf = wire::encode(&t);

    let mut ui = Ui::new(2.0);
    ui.apply(&buf).unwrap();
    ui.render(Size::new(800.0, 600.0));
    let all = resolved(ui.scene());
    let red: Vec<_> = all.iter().filter(|p| p.color == 0xFF00_00FF).collect();
    let green: Vec<_> = all.iter().filter(|p| p.color == 0x00FF_00FF).collect();
    assert_eq!(red.len(), 1, "half bar in the bar color");
    assert_eq!(green.len(), 1, "max bar in the highlight color");
    // scale=2: the 40pt-tall max bar lands 80 device px tall.
    assert_eq!(green[0].bounds.size.height, 80.0);
}

/// A surface of an unregistered kind paints only its box.
#[test]
fn surface_without_painter_paints_background() {
    let mut t = Transaction::new(1);
    let mut s = taffy::Style::default();
    s.size = taffy::Size {
        width: taffy::Dimension::length(50.0),
        height: taffy::Dimension::length(50.0),
    };
    let st1 = t.style(&s);
    t.create(0, NodeKind::Surface);
    t.push(Mutation::Layout { id: 0, style: st1 });
    t.paint(0, Some(0x1122_3344), None, None);
    t.surface(0, 99, [0; 4]);
    t.place(NIL, 0, NIL);
    let buf = wire::encode(&t);

    let mut ui = Ui::new(1.0);
    ui.apply(&buf).unwrap();
    ui.render(Size::new(800.0, 600.0));
    assert!(resolved(ui.scene()).iter().any(|p| p.color == 0x1122_3344));
}

// ---- accessibility ----

/// The retained tree projects onto an AccessKit tree: roles, names,
/// values, bounds, and focus all come from host state.
#[test]
fn a11y_tree_maps_roles_names_focus() {
    use crate::a11y::aid;
    use accesskit::{Action, ActionRequest, Role, TreeId};

    let mut t = Transaction::new(1);
    let mut s = taffy::Style::default();
    s.display = taffy::Display::Flex;
    s.flex_direction = taffy::FlexDirection::Column;
    s.size = taffy::Size {
        width: taffy::Dimension::percent(1.0),
        height: taffy::Dimension::percent(1.0),
    };
    let st1 = t.style(&s);
    t.create(0, NodeKind::View);
    t.push(Mutation::Layout { id: 0, style: st1 });
    t.place(NIL, 0, NIL);
    t.label(0, "root container");
    t.create(1, NodeKind::Text);
    t.text(1, "hello", 14.0, 0xFFFF_FFFF);
    t.role(1, crate::mutation::Role::Label);
    t.place(0, 1, NIL);
    t.create(2, NodeKind::Input);
    t.input_config(2, 14.0, "type here", false);
    t.role(2, crate::mutation::Role::TextInput);
    t.place(0, 2, NIL);
    t.seq = 1;
    let buf = wire::encode(&t);

    let mut ui = Ui::new(1.0);
    ui.apply(&buf).unwrap();
    ui.render(Size::new(400.0, 300.0));
    let tree = ui.a11y_tree(Size::new(400.0, 300.0));
    let find = |id: u32| {
        tree.nodes
            .iter()
            .find(|(n, _)| *n == aid(NodeId(id)))
            .map(|(_, n)| n)
    };

    assert_eq!(find(0).unwrap().role(), Role::GenericContainer);
    assert_eq!(find(0).unwrap().label(), Some("root container"));
    assert_eq!(find(1).unwrap().role(), Role::Label);
    assert_eq!(find(1).unwrap().value(), Some("hello"));
    let input = find(2).unwrap();
    assert_eq!(input.role(), Role::TextInput);
    assert_eq!(input.placeholder(), Some("type here"));
    assert!(input.supports_action(Action::Focus));
    assert!(input.supports_action(Action::ReplaceSelectedText));
    // A laid-out node reports logical bounds.
    assert!(find(1).unwrap().bounds().unwrap().y1 > 0.0);

    // Focus action moves native focus.
    ui.a11y_action(&ActionRequest {
        action: Action::Focus,
        target_tree: TreeId::ROOT,
        target_node: aid(NodeId(2)),
        data: None,
    });
    assert_eq!(ui.focused(), Some(NodeId(2)));
    let tree = ui.a11y_tree(Size::new(400.0, 300.0));
    assert_eq!(tree.focus, aid(NodeId(2)));
}

/// An assistive-tech Click activates the pressable (no pointer
/// events, no hit test); on a plain node it does nothing.
#[test]
fn a11y_click_activates() {
    use crate::a11y::aid;
    use crate::mutation::press;
    use accesskit::{Action, ActionRequest, TreeId};

    let mut t = Transaction::new(1);
    let mut s = taffy::Style::default();
    s.size = taffy::Size {
        width: taffy::Dimension::length(50.0),
        height: taffy::Dimension::length(50.0),
    };
    let st1 = t.style(&s);
    t.create(0, NodeKind::View);
    t.push(Mutation::Layout { id: 0, style: st1 });
    let listeners = mask::POINTER_DOWN | mask::POINTER_UP | mask::ACTIVATE;
    t.interaction_press(0, listeners, false, press::PRESSABLE);
    t.place(NIL, 0, NIL);
    t.seq = 1;
    let buf = wire::encode(&t);

    let mut ui = Ui::new(1.0);
    ui.apply(&buf).unwrap();
    ui.render(Size::new(400.0, 300.0));
    ui.take_events();
    let click = |ui: &mut Ui| {
        ui.a11y_action(&ActionRequest {
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node: aid(NodeId(0)),
            data: None,
        });
        ui.take_events().iter().map(|e| e.kind).collect::<Vec<u8>>()
    };
    assert_eq!(click(&mut ui), [out_kind::ACTIVATE]);
    let mut t = Transaction::new(2);
    t.interaction(0, listeners, false);
    ui.apply_txn(&t).unwrap();
    assert_eq!(click(&mut ui), []);
}

/// ScrollIntoView walks ancestors and clamps offsets so the target
/// lands inside the container's clip box.
#[test]
fn a11y_scroll_into_view() {
    use crate::a11y::aid;
    use accesskit::{Action, ActionRequest, TreeId};

    let mut t = Transaction::new(1);
    let mut outer = taffy::Style::default();
    outer.display = taffy::Display::Flex;
    outer.overflow = taffy::Point {
        x: taffy::Overflow::Scroll,
        y: taffy::Overflow::Scroll,
    };
    outer.size = taffy::Size {
        width: taffy::Dimension::length(100.0),
        height: taffy::Dimension::length(100.0),
    };
    let st1 = t.style(&outer);
    let mut inner = taffy::Style::default();
    inner.size = taffy::Size {
        width: taffy::Dimension::length(100.0),
        height: taffy::Dimension::length(1000.0),
    };
    let st2 = t.style(&inner);
    let mut leaf = taffy::Style::default();
    leaf.position = taffy::Position::Absolute;
    leaf.inset = taffy::Rect {
        top: taffy::LengthPercentageAuto::length(900.0),
        left: taffy::LengthPercentageAuto::length(0.0),
        ..taffy::Rect::auto()
    };
    leaf.size = taffy::Size {
        width: taffy::Dimension::length(50.0),
        height: taffy::Dimension::length(50.0),
    };
    let st3 = t.style(&leaf);
    t.create(0, NodeKind::View);
    t.push(Mutation::Layout { id: 0, style: st1 });
    t.place(NIL, 0, NIL);
    t.create(1, NodeKind::View);
    t.push(Mutation::Layout { id: 1, style: st2 });
    t.place(0, 1, NIL);
    t.create(2, NodeKind::View);
    t.push(Mutation::Layout { id: 2, style: st3 });
    t.place(1, 2, NIL);
    t.seq = 1;
    let buf = wire::encode(&t);

    let mut ui = Ui::new(1.0);
    ui.apply(&buf).unwrap();
    ui.render(Size::new(400.0, 300.0));
    assert_eq!(ui.abs_rect(NodeId(2)).origin.y, 900.0);

    ui.a11y_action(&ActionRequest {
        action: Action::ScrollIntoView,
        target_tree: TreeId::ROOT,
        target_node: aid(NodeId(2)),
        data: None,
    });
    // Node's bottom (950) must land at the container's bottom (100).
    assert_eq!(ui.abs_rect(NodeId(2)).origin.y, 50.0);
}

// ---- executor and wire ----

fn column() -> taffy::Style {
    taffy::Style {
        flex_direction: taffy::FlexDirection::Column,
        size: taffy::Size {
            width: taffy::Dimension::percent(1.0),
            height: taffy::Dimension::percent(1.0),
        },
        ..taffy::Style::default()
    }
}

/// Every mutation survives encode -> decode unchanged, including the
/// per-transaction style and span tables.
#[test]
fn wire_roundtrip_is_exact() {
    let mut t = Transaction::new(42);
    let payload = [1u8, 2, 3, 4, 5];
    t.create(0, NodeKind::View)
        .create(1, NodeKind::Text)
        .create(2, NodeKind::Input)
        .create(3, NodeKind::Surface)
        .layout(0, &column())
        .layout(1, &column())
        .place(NIL, 0, NIL)
        .append(0, 1)
        .place(0, 2, 1)
        .transform(0, craie_core::Affine::rotate(0.5))
        .opacity(0, 0.25)
        .paint(0, Some(0x1122_33FF), Some(4.0), Some((0xFF00_00FF, 1.5)))
        .paragraph(
            1,
            "héllo wörld",
            &[
                TextSpan::default(),
                TextSpan {
                    start: 6,
                    font_size: 20.0,
                    color: 0xFF,
                    weight: 700,
                    italic: true,
                    ..TextSpan::default()
                },
            ],
        )
        .input_config(2, 15.0, "type", true)
        .role(2, Role::TextInput)
        .label(0, "root")
        .interaction(0, mask::POINTER_DOWN, true)
        .surface(3, 7, [1, 2, 3, 4])
        .payload(3, &payload)
        .command(2, Command::SetText("seed".into()))
        .command(0, Command::ScrollTo(1.0, 2.0))
        .command(2, Command::Focus)
        .command(2, Command::Blur)
        .create(4, NodeKind::List)
        .list_config(
            4,
            120.0,
            32.0,
            &[crate::mutation::ItemTemplate {
                base: 12.0,
                inset: 16.0,
                font_size: 14.0,
            }],
        )
        .list_splice(
            4,
            0,
            0,
            &[
                crate::mutation::ItemDesc {
                    template: 0,
                    text_len: 42,
                    id: 7,
                    unchanged: false,
                },
                crate::mutation::ItemDesc {
                    template: 3,
                    text_len: 70_000,
                    id: NIL,
                    unchanged: false,
                },
            ],
        )
        .list_index(1, 1)
        .scroll_anchor(0, crate::mutation::Anchor::StickToEnd)
        .z(0, -7)
        .layer(4, 0)
        .detach(2)
        .remove(3);
    let buf = wire::encode(&t);
    let back = wire::decode(&buf).unwrap();
    assert_eq!(back.seq, 42);
    assert_eq!(back.styles, t.styles);
    assert_eq!(back.spans, t.spans);
    assert_eq!(back.mutations, t.mutations);
    // Identical styles share one table row.
    assert_eq!(t.styles.len(), 1);
}

#[test]
fn wire_rejects_garbage_and_bad_refs() {
    assert!(matches!(wire::decode(&[]), Err(WireError::Truncated)));
    assert!(matches!(wire::decode(&[0; 28]), Err(WireError::BadMagic)));
    let mut t = Transaction::new(1);
    t.push(Mutation::Layout { id: 0, style: 3 });
    let buf = wire::encode(&t);
    assert!(matches!(
        wire::decode(&buf),
        Err(WireError::BadRef("style"))
    ));
}

/// A failing transaction applies nothing, even after valid mutations.
#[test]
fn invalid_transaction_is_atomic() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .append(NIL, 0)
        .create(1, NodeKind::Text);
    // Paint on a text node is invalid.
    t.fill(1, 0xFF);
    assert!(ui.apply_txn(&t).is_err());
    assert_eq!(ui.host.len(), 0);
    assert!(ui.host.children(crate::host::ROOT).is_empty());

    for bad in [
        {
            let mut t = Transaction::new(2);
            t.create(0, NodeKind::View).append(0, 0);
            t
        },
        {
            let mut t = Transaction::new(2);
            t.create(0, NodeKind::Text).paragraph(
                0,
                "ab",
                &[TextSpan {
                    start: 1,
                    ..TextSpan::default()
                }],
            );
            t
        },
        {
            let mut t = Transaction::new(2);
            t.create(0, NodeKind::Text).paragraph(
                0,
                "é",
                &[
                    TextSpan::default(),
                    TextSpan {
                        start: 1,
                        ..TextSpan::default()
                    },
                ],
            );
            t
        },
        {
            let mut t = Transaction::new(2);
            t.create(0, NodeKind::View).opacity(0, 1.5);
            t
        },
        {
            let mut t = Transaction::new(2);
            t.create(0, NodeKind::View).payload(0, &[1]);
            t
        },
    ] {
        assert!(ui.apply_txn(&bad).is_err(), "{:?}", bad.mutations);
        assert_eq!(ui.host.len(), 0);
    }
}

/// Mount a column with one text child; returns the ui.
fn text_ui(color: u32, size: f32) -> Ui {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .layout(0, &column())
        .append(NIL, 0)
        .create(1, NodeKind::Text)
        .text(1, "hello world", size, color)
        .append(0, 1);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    ui
}

/// A color-only paragraph change is a paint change: no text metrics
/// revision, no layout invalidation. A size change reshapes.
#[test]
fn paragraph_changes_classify() {
    let mut ui = text_ui(0xFFFF_FFFF, 14.0);
    let before = ui.host.revs;
    let mut t = Transaction::new(2);
    t.text(1, "hello world", 14.0, 0xFF00_00FF);
    ui.apply_txn(&t).unwrap();
    assert_eq!(ui.host.revs.text_metrics, before.text_metrics);
    assert_eq!(ui.host.revs.layout_input, before.layout_input);
    assert!(ui.host.dirty.layout.is_empty());
    assert!(ui.host.revs.paint > before.paint);
    assert!(ui.needs_paint());

    let before = ui.host.revs;
    let mut t = Transaction::new(3);
    t.text(1, "hello world", 18.0, 0xFF00_00FF);
    ui.apply_txn(&t).unwrap();
    assert!(ui.host.revs.text_metrics > before.text_metrics);
    assert!(!ui.host.dirty.layout.is_empty());
}

/// Removing and recreating an id in one transaction bumps the
/// generation, and outbound events carry it.
#[test]
fn recycled_ids_carry_new_generation() {
    let mut ui = Ui::new(1.0);
    let mut s = taffy::Style::default();
    s.size = taffy::Size {
        width: taffy::Dimension::length(50.0),
        height: taffy::Dimension::length(50.0),
    };
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .layout(0, &s)
        .append(NIL, 0)
        .interaction(0, mask::POINTER_DOWN, false);
    ui.apply_txn(&t).unwrap();
    let gen0 = ui.host.node(NodeId(0)).unwrap().generation;
    let mut t = Transaction::new(2);
    t.remove(0)
        .create(0, NodeKind::View)
        .layout(0, &s)
        .append(NIL, 0)
        .interaction(0, mask::POINTER_DOWN, false);
    ui.apply_txn(&t).unwrap();
    let gen1 = ui.host.node(NodeId(0)).unwrap().generation;
    assert_eq!(gen1, gen0.wrapping_add(1));
    ui.render(Size::new(200.0, 200.0));
    ui.take_events();
    ui.dispatch(&crate::events::Event::PointerDown {
        x: 10.0,
        y: 10.0,
        button: crate::events::Button::Primary,
        mods: Mods::default(),
    });
    let ev = ui.take_events();
    assert_eq!(ev.len(), 1);
    assert_eq!((ev[0].node, ev[0].generation), (0, gen1));
}

/// Spatial ops never touch layout inputs.
#[test]
fn spatial_does_not_invalidate_layout() {
    let mut ui = text_ui(0xFFFF_FFFF, 14.0);
    let before = ui.host.revs;
    let mut t = Transaction::new(2);
    t.transform(0, craie_core::Affine::translate(10.0, 0.0))
        .opacity(0, 0.5);
    ui.apply_txn(&t).unwrap();
    assert_eq!(ui.host.revs.layout_input, before.layout_input);
    assert!(ui.host.dirty.layout.is_empty());
    assert!(ui.host.revs.transform > before.transform);
    assert_eq!(ui.host.spatial[0].opacity, 0.5);
}

/// Roles come from the role field only: listeners never make a button.
#[test]
fn roles_are_explicit() {
    use crate::a11y::aid;
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .append(NIL, 0)
        .interaction(0, mask::POINTER_DOWN | mask::POINTER_UP, false)
        .create(1, NodeKind::View)
        .append(0, 1)
        .role(1, Role::Button);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(100.0, 100.0));
    let tree = ui.a11y_tree(Size::new(100.0, 100.0));
    let role = |id: u32| {
        tree.nodes
            .iter()
            .find(|(n, _)| *n == aid(NodeId(id)))
            .unwrap()
            .1
            .role()
    };
    assert_eq!(role(0), accesskit::Role::GenericContainer);
    assert_eq!(role(1), accesskit::Role::Button);
}

// ---- review regressions ----

fn sized(w: f32, h: f32) -> taffy::Style {
    taffy::Style {
        size: taffy::Size {
            width: taffy::Dimension::length(w),
            height: taffy::Dimension::length(h),
        },
        flex_shrink: 0.0,
        ..taffy::Style::default()
    }
}

#[test]
fn node_ids_are_bounded() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(crate::host::MAX_NODES, NodeKind::View);
    assert!(ui.apply_txn(&t).is_err());
    assert_eq!(
        ui.host.slot_count(),
        0,
        "a rejected id must not grow the stores"
    );
}

/// Removing and recreating a parent orphans its old children: placing
/// before one of them under the new parent is invalid.
#[test]
fn removed_parent_orphans_children_in_validation() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .append(NIL, 0)
        .create(1, NodeKind::View)
        .append(0, 1)
        .create(2, NodeKind::View);
    ui.apply_txn(&t).unwrap();
    let mut t = Transaction::new(2);
    t.remove(0)
        .create(0, NodeKind::View)
        .append(NIL, 0)
        .place(0, 2, 1);
    assert!(ui.apply_txn(&t).is_err());
}

/// A radius change on a clipping node reshapes its children's clip.
#[test]
fn radius_change_updates_child_clip() {
    let mut ui = Ui::new(1.0);
    let mut clip = sized(100.0, 100.0);
    clip.overflow = taffy::Point {
        x: taffy::Overflow::Hidden,
        y: taffy::Overflow::Hidden,
    };
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .layout(0, &clip)
        .fill(0, 0x2233_44FF)
        .append(NIL, 0);
    t.create(1, NodeKind::View)
        .layout(1, &sized(100.0, 100.0))
        .fill(1, 0xFF00_00FF)
        .append(0, 1);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(200.0, 200.0));
    let mut t = Transaction::new(2);
    t.paint(0, None, Some(20.0), None);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(200.0, 200.0));
    let child = resolved(ui.scene())
        .into_iter()
        .find(|p| p.color == 0xFF00_00FF)
        .unwrap();
    assert_eq!(child.clip_radius, 20.0);
}

/// `overflow: { x: visible, y: hidden }` clips only the y axis, for
/// drawing and for hits.
#[test]
fn clip_applies_per_axis() {
    let mut ui = Ui::new(1.0);
    let mut clip = sized(100.0, 100.0);
    clip.overflow = taffy::Point {
        x: taffy::Overflow::Visible,
        y: taffy::Overflow::Hidden,
    };
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View).layout(0, &clip).append(NIL, 0);
    t.create(1, NodeKind::View)
        .layout(1, &sized(300.0, 300.0))
        .fill(1, 0xFF00_00FF)
        .append(0, 1);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 400.0));
    // Past the right edge: visible and hittable. Past the bottom: not.
    assert_eq!(ui.hit_test(250.0, 50.0), Some(NodeId(1)));
    assert_eq!(ui.hit_test(50.0, 250.0), None);
    // The clip record bounds y only; x is open, not a large finite box.
    let clip = ui.scene().clips.get(0);
    assert_eq!(clip.open, [true, false]);
    assert_eq!(clip.rect.size.height, 100.0);
    // The child is drawn: an open-axis clip must not cull it.
    let drawn = ui.scene().resolve_drawn(&|r| r.0 as u64);
    assert!(drawn.iter().any(|p| p.color == 0xFF00_00FF), "child culled");
}

/// A rounded clip: a point in the cut corner does not hit the child.
#[test]
fn hit_test_follows_rounded_clip() {
    let mut ui = Ui::new(1.0);
    let mut clip = sized(100.0, 100.0);
    clip.overflow = taffy::Point {
        x: taffy::Overflow::Hidden,
        y: taffy::Overflow::Hidden,
    };
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .layout(0, &clip)
        .paint(0, None, Some(40.0), None)
        .append(NIL, 0);
    t.create(1, NodeKind::View)
        .layout(1, &sized(100.0, 100.0))
        .append(0, 1);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(200.0, 200.0));
    assert_eq!(ui.hit_test(50.0, 50.0), Some(NodeId(1)));
    assert_eq!(
        ui.hit_test(2.0, 2.0),
        None,
        "the corner is outside the rounded clip"
    );
}

/// A scroll command in the batch that grows the content clamps against
/// the new extent.
#[test]
fn scroll_command_clamps_after_its_batch_layout() {
    let mut ui = Ui::new(1.0);
    let mut s = sized(100.0, 100.0);
    s.overflow = taffy::Point {
        x: taffy::Overflow::Scroll,
        y: taffy::Overflow::Scroll,
    };
    s.flex_direction = taffy::FlexDirection::Column;
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View).layout(0, &s).append(NIL, 0);
    t.create(1, NodeKind::View)
        .layout(1, &sized(100.0, 100.0))
        .append(0, 1);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(200.0, 200.0));
    let mut t = Transaction::new(2);
    t.layout(1, &sized(100.0, 300.0))
        .command(0, Command::ScrollTo(0.0, 150.0));
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(200.0, 200.0));
    assert_eq!(ui.scroll_offset(NodeId(0)), [0.0, 150.0]);
}

/// Registering a painter after its surfaces rendered redraws them.
#[test]
fn registering_a_painter_rebuilds_its_surfaces() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::Surface)
        .layout(0, &sized(50.0, 50.0))
        .surface(0, 42, [0; 4])
        .append(NIL, 0);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(200.0, 200.0));
    assert!(resolved(ui.scene()).is_empty());
    ui.register_surface(
        42,
        Box::new(|_, r, out| {
            out.push(surface::Quad {
                x: r.origin.x,
                y: r.origin.y,
                w: 10.0,
                h: 10.0,
                color: 0x00FF_00FF,
                ..surface::Quad::default()
            })
        }),
    );
    ui.render(Size::new(200.0, 200.0));
    assert!(resolved(ui.scene()).iter().any(|p| p.color == 0x00FF_00FF));
}

/// Batch-local links count too: a child placed under a parent that the
/// same batch then removes is an orphan.
#[test]
fn batch_links_orphan_on_parent_removal() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .append(NIL, 0)
        .create(1, NodeKind::View)
        .create(2, NodeKind::View);
    ui.apply_txn(&t).unwrap();
    let mut t = Transaction::new(2);
    t.append(0, 1)
        .remove(0)
        .create(0, NodeKind::View)
        .append(NIL, 0)
        .place(0, 2, 1);
    assert!(ui.apply_txn(&t).is_err());
    // Re-linking after the recreation is valid.
    let mut t = Transaction::new(3);
    t.remove(0)
        .create(0, NodeKind::View)
        .append(NIL, 0)
        .append(0, 1)
        .place(0, 2, 1);
    ui.apply_txn(&t).unwrap();
    assert_eq!(ui.host.children(NodeId(0)), [NodeId(2), NodeId(1)]);
}

/// Ids stay dense: a create far past the slots in use is invalid.
#[test]
fn node_ids_stay_dense() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(1_000_000, NodeKind::View);
    assert!(ui.apply_txn(&t).is_err());
    assert_eq!(ui.host.slot_count(), 0);
    let mut t = Transaction::new(2);
    for id in 0..100 {
        t.create(id, NodeKind::View);
    }
    ui.apply_txn(&t).unwrap();
    // Many creates that each jump the slack still fail: the limit
    // counts nodes created, not ids reached.
    let mut t = Transaction::new(3);
    for k in 1..=4u32 {
        t.create(100 + k * 4000, NodeKind::View);
    }
    assert!(ui.apply_txn(&t).is_err());
}

// ---- Astra review, round 1 ----

/// A fixed-size text node skips measurement; its update must still
/// redraw the new paragraph.
#[test]
fn fixed_size_text_redraws_after_update() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::Text)
        .layout(0, &sized(200.0, 40.0))
        .text(0, "A", 20.0, 0xFFFF_FFFF)
        .append(NIL, 0);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(300.0, 100.0));
    let before = glyphs(ui.scene());
    let mut t = Transaction::new(2);
    t.text(0, "BBBB", 20.0, 0xFFFF_FFFF);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(300.0, 100.0));
    assert_eq!(before, 1);
    assert_eq!(glyphs(ui.scene()), 4, "the old paragraph is still drawn");
}

/// A lone SetText command makes an idle UI owe a paint.
#[test]
fn set_text_command_needs_paint() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::Input)
        .layout(0, &sized(200.0, 30.0))
        .input_config(0, 16.0, "", false)
        .append(NIL, 0);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(300.0, 100.0));
    assert!(!ui.needs_paint());
    let mut t = Transaction::new(2);
    t.command(0, Command::SetText("new".into()));
    ui.apply_txn(&t).unwrap();
    assert!(ui.needs_paint(), "the redraw gate must see the command");
    ui.render(Size::new(300.0, 100.0));
    assert_eq!(glyphs(ui.scene()), 3);
}

/// Malformed style tags and mask bits reject the whole transaction; host
/// state and the applied sequence stay unchanged.
#[test]
fn malformed_style_rejects_transaction() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View).append(NIL, 0);
    ui.apply_txn(&t).unwrap();
    let before = *ui.host.style(NodeId(0));
    let seq = ui.seq;
    let mut t = Transaction::new(7);
    t.layout(0, &sized(10.0, 10.0));
    let good = wire::encode(&t);
    // The style table starts after the 28-byte header (no strings). Its
    // first 8 bytes are the mask; the next byte is the DISPLAY tag.
    let mut bad_tag = good.clone();
    bad_tag[36] = 2;
    let mut bad_mask = good.clone();
    bad_mask[28 + 7] |= 0x80;
    for buf in [bad_tag, bad_mask] {
        assert!(ui.apply(&buf).is_err());
        assert_eq!(*ui.host.style(NodeId(0)), before);
        assert_eq!(ui.seq, seq);
    }
    assert!(ui.apply(&good).is_ok());
}

/// A View that sends no style lays its children out in a column, as in
/// React Native; so does a partial style and a reset.
#[test]
fn views_default_to_column() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View).append(NIL, 0);
    for i in 1..=2 {
        t.create(i, NodeKind::View)
            .layout(i, &sized(20.0, 20.0))
            .append(0, i);
    }
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(200.0, 200.0));
    assert_eq!(ui.layouts.data(NodeId(2)).rect.origin.y, 20.0);
    assert_eq!(ui.layouts.data(NodeId(2)).rect.origin.x, 0.0);
    // Reset to defaults: still a column.
    let mut t = Transaction::new(2);
    t.push(Mutation::Layout { id: 0, style: NIL });
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(200.0, 200.0));
    assert_eq!(ui.layouts.data(NodeId(2)).rect.origin.y, 20.0);
}

/// Children do not shrink by default (React Native): a fixed box in a
/// row keeps its size next to wide text; the text shrinks and wraps
/// only when it asks to.
#[test]
fn views_default_to_no_shrink() {
    let mut ui = Ui::new(1.0);
    let mut row = taffy::Style {
        flex_direction: taffy::FlexDirection::Row,
        ..sized(200.0, 100.0)
    };
    row.flex_shrink = 0.0;
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View).layout(0, &row).append(NIL, 0);
    t.create(1, NodeKind::View)
        .layout(1, &sized(18.0, 18.0))
        .append(0, 1);
    t.create(2, NodeKind::View).append(0, 2);
    t.create(3, NodeKind::Text)
        .text(3, WORDS, 14.0, 0xFFFF_FFFF)
        .append(2, 3);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    assert_eq!(ui.layouts.data(NodeId(1)).rect.size.width, 18.0);
    let text_w = ui.layouts.data(NodeId(2)).rect.size.width;
    assert!(text_w > 200.0, "no shrink: the text overflows ({text_w})");

    let mut t = Transaction::new(2);
    t.layout(
        2,
        &taffy::Style {
            flex_direction: taffy::FlexDirection::Column,
            flex_grow: 1.0,
            flex_shrink: 1.0,
            ..taffy::Style::default()
        },
    );
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    assert_eq!(ui.layouts.data(NodeId(1)).rect.size.width, 18.0);
    assert_eq!(ui.layouts.data(NodeId(2)).rect.size.width, 182.0);
}

/// React Native flex defaults (no grow, no shrink, auto basis) hold for
/// every way a node gets its style: none sent, a partial wire style
/// (only the fields the app set, as the JS encoder sends), and a reset
/// to NIL after explicit values. Checked on the stored style and on
/// layout: each child holds an 80-wide box, so its auto basis is 80; in
/// a row too narrow for all three none shrinks, in a wide row none
/// grows.
#[test]
fn rn_flex_defaults_matrix() {
    use crate::wire::field;
    let flex = |s: &craie_layout::LayoutRow| {
        let t = s.to_taffy();
        (t.flex_grow, t.flex_shrink, t.flex_basis)
    };
    let rn = (0.0, 0.0, taffy::Dimension::auto());
    let row = |w: f32| taffy::Style {
        flex_direction: taffy::FlexDirection::Row,
        ..sized(w, 40.0)
    };
    let child = sized(80.0, 20.0);

    // Mount: a row; node 1 sends no style, 2 a partial style (size
    // only), 3 explicit flex values that are reset to NIL below.
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .layout(0, &row(100.0))
        .append(NIL, 0);
    t.create(1, NodeKind::View).append(0, 1);

    let partial = t.style(&child);
    t.create(2, NodeKind::View)
        .push(Mutation::Layout {
            id: 2,
            style: partial,
        })
        .append(0, 2);
    t.create(3, NodeKind::View)
        .layout(
            3,
            &taffy::Style {
                flex_grow: 1.0,
                flex_shrink: 1.0,
                flex_basis: taffy::Dimension::length(10.0),
                ..child.clone()
            },
        )
        .append(0, 3);
    // An 80-wide box inside each of 1 and 3 (a distinct style row).
    for (id, parent) in [(4, 1), (5, 3)] {
        t.create(id, NodeKind::View)
            .layout(id, &sized(80.0, 10.0))
            .append(parent, id);
    }
    let mut buf = wire::encode(&t);
    // Replace the full style record of `partial` with a size-only one.
    let at = 28
        + (0..partial as usize)
            .map(|i| {
                let mut v = Vec::new();
                wire::put_style(&mut v, &t.styles[i]);
                v.len()
            })
            .sum::<usize>();
    let mut full = Vec::new();
    wire::put_style(&mut full, &child);
    let mut part = Vec::new();
    wire::put_style_masked(&mut part, &child, field::SIZE);
    buf.splice(at..at + full.len(), part);
    let mut ui = Ui::new(1.0);
    ui.apply(&buf).unwrap();
    assert_eq!(flex(&ui.host.layout[1]), rn, "omitted");
    assert_eq!(flex(&ui.host.layout[2]), rn, "partial");
    assert_eq!(
        ui.host.layout[2].size(),
        child.size,
        "partial keeps its fields"
    );
    assert_ne!(flex(&ui.host.layout[3]), rn, "explicit");

    let mut t = Transaction::new(2);
    t.push(Mutation::Layout { id: 3, style: NIL });
    ui.apply_txn(&t).unwrap();
    assert_eq!(flex(&ui.host.layout[3]), rn, "reset");

    ui.render(Size::new(400.0, 300.0));
    let w = |ui: &Ui, id: u32| ui.layouts.data(NodeId(id)).rect.size.width;
    assert_eq!(
        (w(&ui, 1), w(&ui, 2), w(&ui, 3)),
        (80.0, 80.0, 80.0),
        "no shrink"
    );
    let mut t = Transaction::new(3);
    t.layout(0, &row(400.0));
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    assert_eq!(
        (w(&ui, 1), w(&ui, 2), w(&ui, 3)),
        (80.0, 80.0, 80.0),
        "no grow"
    );
}

/// A transformed subtree places its chunks fractionally: slow motion
/// moves the drawn text by fractions, not in whole-pixel steps. Chunks
/// in untransformed space still snap.
#[test]
fn transformed_chunks_move_fractionally() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View).append(NIL, 0);
    t.create(1, NodeKind::Text)
        .text(1, "move", 16.0, 0xFFFF_FFFF)
        .append(0, 1);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(300.0, 100.0));
    let x0 = resolved(ui.scene())[0].bounds.origin.x;
    assert_eq!(x0, x0.round(), "static text snaps");
    let mut xs = Vec::new();
    for step in 1..=5 {
        let mut t = Transaction::new(1 + step);
        t.transform(1, craie_core::Affine::translate(step as f32 * 0.1, 0.0));
        ui.apply_txn(&t).unwrap();
        ui.render(Size::new(300.0, 100.0));
        xs.push(resolved(ui.scene())[0].bounds.origin.x - x0);
    }
    for (i, dx) in xs.iter().enumerate() {
        let want = (i + 1) as f32 * 0.1;
        assert!(
            (dx - want).abs() < 1e-3,
            "step {i}: moved {dx}, want {want}"
        );
    }
    // At rest the transformed chunk snaps: its origin, at 0.5 px, rounds
    // to even (0), and the glyph returns to its static position.
    ui.set_time(crate::ui::SETTLE_SECS * 1.5);
    assert!(ui.settle());
    ui.render(Size::new(300.0, 100.0));
    let x = resolved(ui.scene())[0].bounds.origin.x;
    assert_eq!(x, x0 + 0.5f32.round_ties_even(), "at rest");
}

/// Unknown op-mask bits (paint, spatial) reject the whole transaction.
#[test]
fn unknown_op_mask_bits_reject() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View).append(NIL, 0);
    ui.apply_txn(&t).unwrap();
    let seq = ui.seq;
    let fill = ui.host.paint[0].fill;
    for (tag_op, t) in [
        (wire::op::PAINT, {
            let mut t = Transaction::new(9);
            t.fill(0, 0xFF00_00FF);
            t
        }),
        (wire::op::SPATIAL, {
            let mut t = Transaction::new(9);
            t.opacity(0, 0.5);
            t
        }),
    ] {
        let mut buf = wire::encode(&t);
        // Header 28 bytes, no tables: op tag, u32 id, then the mask.
        assert_eq!(buf[28], tag_op);
        buf[33] |= 0x80;
        assert!(ui.apply(&buf).is_err());
        assert_eq!(
            (ui.seq, ui.host.paint[0].fill, ui.host.spatial[0].opacity),
            (seq, fill, 1.0)
        );
    }
}

/// Scroll content moves by fractions at 1x and 2x, also under a
/// transformed ancestor: no whole-pixel steps.
#[test]
fn scroll_content_moves_fractionally() {
    for (scale, transformed) in [(1.0f32, false), (2.0, false), (2.0, true)] {
        let mut ui = Ui::new(scale);
        let mut s = sized(200.0, 100.0);
        s.overflow = taffy::Point {
            x: taffy::Overflow::Scroll,
            y: taffy::Overflow::Scroll,
        };
        let mut t = Transaction::new(1);
        t.create(0, NodeKind::View).append(NIL, 0);
        if transformed {
            t.transform(0, craie_core::Affine::translate(3.0, 5.0));
        }
        t.create(1, NodeKind::View).layout(1, &s).append(0, 1);
        t.create(2, NodeKind::View)
            .layout(2, &sized(200.0, 1000.0))
            .fill(2, 0xFF00_00FF)
            .append(1, 2);
        ui.apply_txn(&t).unwrap();
        ui.render(Size::new(300.0, 300.0));
        let y = |ui: &Ui| {
            resolved(ui.scene())
                .into_iter()
                .find(|p| p.color == 0xFF00_00FF)
                .unwrap()
                .bounds
                .origin
                .y
        };
        // Settle the mount first: content at rest snaps.
        ui.set_time(1.0);
        ui.settle();
        ui.render(Size::new(300.0, 300.0));
        let y0 = y(&ui);
        assert_eq!(y0, y0.round(), "scale {scale}: at rest before motion");
        let mut now = 1.0;
        for step in 1..=4 {
            now += 0.016;
            ui.set_time(now);
            ui.scroll_to(NodeId(1), 0.0, step as f32 * 0.05);
            ui.render(Size::new(300.0, 300.0));
            let dy = y0 - y(&ui);
            let want = step as f32 * 0.05 * scale;
            assert!(
                (dy - want).abs() < 1e-3,
                "scale {scale} step {step}: {dy} vs {want}"
            );
        }
        // Stop half a device pixel off the grid: fractional while moving,
        // snapped (ties to even) once at rest.
        let half = 0.5 / scale;
        now += 0.016;
        ui.set_time(now);
        ui.scroll_to(NodeId(1), 0.0, half);
        ui.render(Size::new(300.0, 300.0));
        let moving = y(&ui);
        assert_eq!(moving, y0 - 0.5, "scale {scale}: moving");
        assert_eq!(ui.next_settle(), Some(now + crate::ui::SETTLE_SECS));
        ui.set_time(now + crate::ui::SETTLE_SECS * 0.5);
        assert!(!ui.settle(), "scale {scale}: still moving");
        ui.set_time(now + crate::ui::SETTLE_SECS * 1.5);
        assert!(ui.settle(), "scale {scale}: settles");
        assert!(ui.needs_paint());
        ui.render(Size::new(300.0, 300.0));
        let settled = y(&ui);
        assert_eq!(settled, moving.round_ties_even(), "scale {scale}: at rest");
        assert_eq!(ui.next_settle(), None);
        // Motion again: fractional again.
        ui.scroll_to(NodeId(1), 0.0, half * 3.0);
        ui.render(Size::new(300.0, 300.0));
        assert_eq!(y(&ui), y0 - 1.5, "scale {scale}: moving again");
    }
}

/// The resolver rounds half-pixel origins to even, as the shader does.
#[test]
fn snapping_rounds_half_to_even() {
    for (x, want) in [
        (10.25f32, 20.0f32),
        (10.75, 22.0),
        (-10.25, -20.0),
        (-10.75, -22.0),
    ] {
        let mut ui = Ui::new(2.0);
        let mut s = sized(20.0, 20.0);
        s.position = taffy::Position::Absolute;
        s.inset.left = taffy::LengthPercentageAuto::length(x);
        let mut t = Transaction::new(1);
        t.create(0, NodeKind::View)
            .layout(0, &sized(100.0, 100.0))
            .append(NIL, 0);
        t.create(1, NodeKind::View)
            .layout(1, &s)
            .fill(1, 0xFF00_00FF)
            .append(0, 1);
        ui.apply_txn(&t).unwrap();
        ui.render(Size::new(300.0, 300.0));
        let b = resolved(ui.scene())
            .into_iter()
            .find(|p| p.color == 0xFF00_00FF)
            .unwrap()
            .bounds;
        assert_eq!(b.origin.x, want, "x {x}");
    }
}

/// A glyph larger than an atlas page renders: it is rasterized smaller,
/// stays resident, and draws at its full size. After eviction it comes
/// back at the same raster size. Font sizes past the bound reject.
#[test]
fn oversized_glyph_renders() {
    use crate::scene::RasterAtlas;
    let mut ui = Ui::new(1.0);
    ui.scene.atlas = RasterAtlas::with_budget(256, 1, 1);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View).append(NIL, 0);
    t.create(1, NodeKind::Text)
        .text(1, "W", 600.0, 0xFFFF_FFFF)
        .append(0, 1);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(1000.0, 1000.0));
    let drawn = resolved(ui.scene());
    let g: Vec<_> = drawn.iter().filter(|p| p.kind == 1).collect();
    assert_eq!(g.len(), 1, "the glyph draws");
    assert!(g[0].bounds.size.width > 256.0 && g[0].bounds.size.height > 256.0);
    let atlas = &ui.scene.atlas;
    assert_eq!(atlas.stats.downscaled, 1);
    assert_eq!(atlas.stats.oversized, 0);
    let id = crate::scene::RasterId(g[0].aux as u32);
    let e = *atlas.entry(id);
    assert!(e.resident);
    assert!(e.w <= 254 && e.h <= 254, "bitmap fits a page");
    assert_eq!(
        (e.quad_w as f32, e.quad_h as f32),
        (g[0].bounds.size.width, g[0].bounds.size.height)
    );

    // A different glyph takes the only page and evicts the first.
    let mut t = Transaction::new(2);
    t.text(1, "M", 600.0, 0xFFFF_FFFF);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(1000.0, 1000.0));
    assert!(!ui.scene.atlas.entry(id).resident, "evicted");
    // Back to the first: re-rasterized at the downscaled size.
    let rerasters = ui.text.cache.stats.rerasters;
    let mut t = Transaction::new(3);
    t.text(1, "W", 600.0, 0xFFFF_FFFF);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(1000.0, 1000.0));
    assert_eq!(ui.text.cache.stats.rerasters, rerasters + 1);
    let back = *ui.scene.atlas.entry(id);
    assert!(back.resident);
    assert_eq!(
        (back.w, back.h, back.quad_w, back.quad_h),
        (e.w, e.h, e.quad_w, e.quad_h)
    );
    assert_eq!(ui.scene.atlas.stats.oversized, 0);

    let mut t = Transaction::new(4);
    t.text(1, "W", crate::executor::MAX_FONT_SIZE * 2.0, 0xFFFF_FFFF);
    assert!(ui.apply_txn(&t).is_err(), "font size past the bound");
}

/// List ops validate against the list's item count as the batch leaves
/// it; any failure rejects the whole transaction.
#[test]
fn list_ops_validate_atomically() {
    use crate::mutation::{Anchor, ItemDesc};
    let item = ItemDesc {
        template: 0,
        text_len: 5,
        id: NIL,
        unchanged: false,
    };
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View).append(NIL, 0);
    t.create(1, NodeKind::List)
        .list_splice(1, 0, 0, &[item; 3])
        // Within the batch: 3 items, so removing 3 at 0 is valid.
        .list_splice(1, 0, 3, &[item; 2])
        .append(0, 1);
    ui.apply_txn(&t).unwrap();
    assert_eq!(ui.host.lists.get(1).unwrap().len(), 2);
    let seq = ui.seq;
    let bad: [BadOp; 6] = [
        // The valid first op leaves 3 items.
        ("splice past the end", |t| {
            t.list_splice(1, 4, 0, &[]);
        }),
        ("remove past the end", |t| {
            t.list_splice(1, 1, 3, &[]);
        }),
        ("splice on a view", |t| {
            t.list_splice(0, 0, 0, &[]);
        }),
        ("config on a view", |t| {
            t.list_config(0, 0.0, 0.0, &[]);
        }),
        ("negative overscan", |t| {
            t.list_config(1, -1.0, 0.0, &[]);
        }),
        ("huge template font", |t| {
            t.list_config(
                1,
                0.0,
                0.0,
                &[crate::mutation::ItemTemplate {
                    base: 0.0,
                    inset: 0.0,
                    font_size: 1.0e9,
                }],
            );
        }),
    ];
    for (what, f) in bad {
        let mut t = Transaction::new(seq + 1);
        // A valid op first: it must not apply either.
        t.list_splice(1, 2, 0, &[item]);
        f(&mut t);
        assert!(ui.apply_txn(&t).is_err(), "{what}");
        assert_eq!(ui.host.lists.get(1).unwrap().len(), 2, "{what}: atomic");
        assert_eq!(ui.seq, seq, "{what}");
    }
    // An unknown anchor policy byte rejects in the decoder.
    let mut t = Transaction::new(9);
    t.scroll_anchor(0, Anchor::None);
    let mut buf = wire::encode(&t);
    *buf.last_mut().unwrap() = 7;
    assert!(wire::decode(&buf).is_err());
}

/// A list removed and re-created at the same id in one batch starts
/// empty in validation too: a splice sized for the old list rejects the
/// whole batch (no partial apply, no panic), through the direct API and
/// the wire; a splice sized for the new list applies.
#[test]
fn list_count_resets_on_id_reuse() {
    use crate::mutation::ItemDesc;
    let item = ItemDesc {
        template: 0,
        text_len: 5,
        id: NIL,
        unchanged: false,
    };
    let batch = |at: u32| {
        let mut t = Transaction::new(2);
        t.create(1, NodeKind::List)
            .list_splice(1, 0, 0, &[item; 3])
            .append(0, 1)
            .remove(1)
            .create(1, NodeKind::List)
            .append(0, 1)
            .list_splice(1, at, 0, &[item]);
        t
    };
    for wire_path in [false, true] {
        let mut ui = Ui::new(1.0);
        let mut t = Transaction::new(1);
        t.create(0, NodeKind::View).append(NIL, 0);
        ui.apply_txn(&t).unwrap();
        let apply = |ui: &mut Ui, t: &Transaction| {
            if wire_path {
                ui.apply(&wire::encode(t)).map(|_| ())
            } else {
                ui.apply_txn(t).map(|_| ())
            }
        };
        assert!(apply(&mut ui, &batch(3)).is_err(), "wire {wire_path}");
        assert!(ui.host.node(NodeId(1)).is_none(), "atomic: nothing applied");
        assert_eq!(ui.seq, 1);
        apply(&mut ui, &batch(0)).unwrap();
        assert_eq!(ui.host.lists.get(1).unwrap().len(), 1);
        ui.render(Size::new(100.0, 100.0));
    }
}

/// Item identities are unique within a list as each batch leaves it:
/// a duplicate within one insertion, against surviving items, or across
/// several splices of one batch rejects the whole batch; moving an id
/// (removed and inserted), repeated NIL, and reuse after the list's id is
/// re-created are valid. Through the direct API and the wire.
#[test]
fn list_identities_are_unique() {
    use crate::mutation::ItemDesc;
    fn item(id: u32) -> ItemDesc {
        ItemDesc {
            template: 0,
            text_len: 5,
            id,
            unchanged: false,
        }
    }
    for wire_path in [false, true] {
        let mut ui = Ui::new(1.0);
        let mut t = Transaction::new(1);
        t.create(0, NodeKind::View).append(NIL, 0);
        t.create(1, NodeKind::List)
            .list_splice(1, 0, 0, &[item(1), item(2), item(3), item(7)])
            .append(0, 1);
        ui.apply_txn(&t).unwrap();
        let apply = |ui: &mut Ui, t: &Transaction| {
            if wire_path {
                ui.apply(&wire::encode(t)).map(|_| ())
            } else {
                ui.apply_txn(t).map(|_| ())
            }
        };
        let bad: [BadOp; 4] = [
            ("twice in one insertion", |t| {
                t.list_splice(1, 0, 0, &[item(9), item(9)]);
            }),
            ("against a surviving item", |t| {
                t.list_splice(1, 0, 1, &[item(7)]);
            }),
            ("across two splices", |t| {
                t.list_splice(1, 0, 0, &[item(9)]);
                t.list_splice(1, 5, 0, &[item(9)]);
            }),
            ("a later splice against an earlier one's survivor", |t| {
                t.list_splice(1, 0, 1, &[item(8)]);
                t.list_splice(1, 4, 0, &[item(3)]);
            }),
        ];
        for (what, f) in bad {
            let mut t = Transaction::new(2);
            // An earlier valid mutation: it must not apply.
            t.fill(0, 0xFF00_00FF);
            f(&mut t);
            assert!(apply(&mut ui, &t).is_err(), "{what} (wire {wire_path})");
            assert_eq!(ui.host.paint[0].fill, 0, "{what}: atomic");
            assert_eq!(ui.host.lists.get(1).unwrap().len(), 4, "{what}: atomic");
        }
        // The index matches the items after every accepted batch.
        let index_ok = |ui: &Ui| {
            let l = ui.host.lists.get(1).unwrap();
            let fresh = crate::list::IdIndex::build(l.items.iter().map(|d| d.id));
            !fresh.has_duplicates() && fresh == l.ids
        };
        assert!(index_ok(&ui));
        // Valid: move id 7 to the front across two splices, repeat NIL.
        let mut t = Transaction::new(3);
        t.list_splice(1, 3, 1, &[])
            .list_splice(1, 0, 0, &[item(7), item(NIL), item(NIL)]);
        apply(&mut ui, &t).unwrap();
        let l = ui.host.lists.get(1).unwrap();
        assert_eq!(
            l.items.iter().map(|d| d.id).collect::<Vec<_>>(),
            [7, NIL, NIL, 1, 2, 3]
        );
        assert!(index_ok(&ui));
        // Re-creating the list's id starts a fresh identity space.
        let mut t = Transaction::new(4);
        t.remove(1)
            .create(1, NodeKind::List)
            .append(0, 1)
            .list_splice(1, 0, 0, &[item(1), item(7)]);
        apply(&mut ui, &t).unwrap();
        assert_eq!(ui.host.lists.get(1).unwrap().len(), 2);
        assert!(index_ok(&ui), "after node reuse");
        ui.render(Size::new(100.0, 100.0));
    }
}

/// Unknown bits in an item's flags byte, or a partial description,
/// reject the whole batch (an earlier valid op must not apply), through
/// the wire and the direct API.
#[test]
fn list_item_flags_are_strict() {
    use crate::mutation::ItemDesc;
    let item = ItemDesc {
        template: 0,
        text_len: 3,
        id: 5,
        unchanged: true,
    };
    for (what, flags, cut) in [
        ("unknown bit", 0x80u8, 0usize),
        ("unknown bit with unchanged", 0x81, 0),
        ("partial description", 0x01, 1),
    ] {
        for wire_path in [false, true] {
            let mut ui = Ui::new(1.0);
            let mut t = Transaction::new(1);
            t.create(0, NodeKind::View).append(NIL, 0);
            t.create(1, NodeKind::List).append(0, 1);
            ui.apply_txn(&t).unwrap();
            let mut bytes = ItemDesc::pack(&[item]);
            *bytes.last_mut().unwrap() = flags;
            bytes.truncate(bytes.len() - cut);
            let mut t = Transaction::new(2);
            t.fill(0, 0xFF00_00FF);
            t.push(Mutation::ListSplice {
                id: 1,
                at: 0,
                remove: 0,
                items: bytes.into(),
            });
            let r = if wire_path {
                ui.apply(&wire::encode(&t)).map(|_| ())
            } else {
                ui.apply_txn(&t)
            };
            assert!(r.is_err(), "{what} (wire {wire_path})");
            assert_eq!(ui.host.paint[0].fill, 0, "{what}: atomic");
            assert!(ui.host.lists.get(1).is_none_or(|l| l.is_empty()));
        }
    }
}

/// The identity index's merge update equals a rebuilt index for bulk
/// removals and additions (the linear path) and for a few (in place).
#[test]
fn id_index_update_matches_rebuild() {
    use crate::list::IdIndex;
    let mut rng = craie_core::rng::Rng::new(11);
    for round in 0..200 {
        let mut ids: Vec<u32> = (0..rng.below(300)).map(|_| rng.below(2_000)).collect();
        ids.sort_unstable();
        ids.dedup();
        let mut ix = IdIndex::build(ids.iter().copied());
        let many = round % 2 == 0;
        let take = if many {
            ids.len() / 2
        } else {
            ids.len().min(3)
        };
        let removed: Vec<u32> = ids
            .iter()
            .copied()
            .filter(|_| rng.chance(0.5))
            .take(take)
            .collect();
        let mut kept: Vec<u32> = ids
            .iter()
            .copied()
            .filter(|i| !removed.contains(i))
            .collect();
        let added: Vec<u32> = (0..if many { 40 } else { 3 })
            .map(|k| 2_000 + k * 7 + rng.below(5))
            .filter(|i| !kept.contains(i))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        ix.update(&removed, &added);
        kept.extend(&added);
        assert_eq!(ix, IdIndex::build(kept), "round {round}");
    }
}

/// A focused input `width` points wide holding `text`.
fn focused_input(text: &str, width: f32, multiline: bool) -> Ui {
    let mut t = Transaction::new(1);
    let s = taffy::Style {
        size: taffy::Size {
            width: taffy::Dimension::length(width),
            height: taffy::Dimension::length(80.0),
        },
        ..taffy::Style::default()
    };
    let st1 = t.style(&s);
    t.create(0, NodeKind::Input);
    t.push(Mutation::Layout { id: 0, style: st1 });
    t.input_config(0, 16.0, "", multiline);
    t.interaction(0, mask::INPUT, true);
    t.place(NIL, 0, NIL);
    t.seq = 1;
    let mut ui = Ui::new(1.0);
    ui.apply(&wire::encode(&t)).unwrap();
    ui.render(Size::new(800.0, 600.0));
    ui.dispatch(&Event::PointerDown {
        x: 5.0,
        y: 5.0,
        button: Button::Primary,
        mods: Mods::default(),
    });
    for ch in text.chars() {
        let s = ch.to_string();
        ui.dispatch(&Event::KeyDown(KeyInput {
            key: Key::Unknown,
            text: Some(s.clone()),
            char: Some(s),
            mods: Mods::default(),
            ..KeyInput::default()
        }));
    }
    ui.render(Size::new(800.0, 600.0));
    ui
}

fn key(ui: &mut Ui, key: Key, ch: Option<&str>, mods: Mods) {
    ui.dispatch(&Event::KeyDown(KeyInput {
        key,
        text: None,
        char: ch.map(str::to_string),
        mods,
        ..KeyInput::default()
    }));
}

/// The command modifier (`mod`): Cmd on macOS, Ctrl elsewhere.
fn meta() -> Mods {
    Mods::from_bits(Mods::COMMAND)
}

/// A composition that replaces a selection is one undo step with its
/// commit: select all of "hello", preedit, the empty preedit platforms
/// send before a commit, commit "かな"; Undo restores "hello".
#[test]
fn composition_undo_restores_the_replaced_text() {
    let mut ui = focused_input("hello", 300.0, false);
    key(&mut ui, Key::Unknown, Some("a"), meta());
    ui.dispatch(&Event::ImePreedit {
        text: "か".into(),
        cursor: Some((3, 3)),
    });
    ui.dispatch(&Event::ImePreedit {
        text: String::new(),
        cursor: None,
    });
    ui.dispatch(&Event::ImeCommit("かな".into()));
    assert_eq!(ui.inputs.text(0), "かな");
    key(&mut ui, Key::Unknown, Some("z"), meta());
    assert_eq!(ui.inputs.text(0), "hello");

    // Copy and caret moves during the composition keep the group.
    let preedit = |ui: &mut Ui, t: &str| {
        ui.dispatch(&Event::ImePreedit {
            text: t.into(),
            cursor: (!t.is_empty()).then_some((t.len(), t.len())),
        })
    };
    let mut ui = focused_input("hello", 300.0, false);
    key(&mut ui, Key::Unknown, Some("a"), meta());
    preedit(&mut ui, "か");
    key(&mut ui, Key::Unknown, Some("c"), meta());
    key(&mut ui, Key::Left, None, Mods::default());
    // Cut with nothing selectable and Paste with an empty clipboard
    // change nothing: the group stays.
    key(&mut ui, Key::Unknown, Some("x"), meta());
    key(&mut ui, Key::Unknown, Some("v"), meta());
    preedit(&mut ui, "");
    ui.dispatch(&Event::ImeCommit("かな".into()));
    key(&mut ui, Key::Unknown, Some("z"), meta());
    assert_eq!(ui.inputs.text(0), "hello", "copy and moves keep the group");

    // Undo during a composition ends the group: the next commit records
    // its own entry and ends the redo history.
    let mut ui = focused_input("hello", 300.0, false);
    key(&mut ui, Key::Unknown, Some("a"), meta());
    preedit(&mut ui, "か");
    key(&mut ui, Key::Unknown, Some("z"), meta());
    assert_eq!(ui.inputs.text(0), "hello");
    preedit(&mut ui, "");
    ui.dispatch(&Event::ImeCommit("x".into()));
    let after_commit = ui.inputs.text(0);
    key(&mut ui, Key::Unknown, Some("z"), meta());
    assert_eq!(
        ui.inputs.text(0),
        "hello",
        "undo returns to before the commit"
    );
    key(
        &mut ui,
        Key::Unknown,
        Some("z"),
        Mods::from_bits(Mods::COMMAND | Mods::SHIFT),
    );
    assert_eq!(
        ui.inputs.text(0),
        after_commit,
        "redo gives the commit, not the old preedit"
    );
}

/// `onChangeText` fires for committed-text changes only: not for caret
/// moves, selection, copy, or a preedit alone.
#[test]
fn change_events_only_for_text_changes() {
    let mut ui = focused_input("ab", 300.0, false);
    let changes = |ui: &mut Ui| -> Vec<String> {
        ui.take_events()
            .into_iter()
            .filter(|e| e.kind == out_kind::CHANGE)
            .map(|e| e.text)
            .collect()
    };
    assert_eq!(changes(&mut ui), ["a", "ab"]);
    key(&mut ui, Key::Left, None, Mods::default());
    key(
        &mut ui,
        Key::Left,
        None,
        Mods {
            shift: true,
            ..Mods::default()
        },
    );
    key(&mut ui, Key::Unknown, Some("c"), meta());
    ui.dispatch(&Event::ImePreedit {
        text: "か".into(),
        cursor: Some((3, 3)),
    });
    assert_eq!(changes(&mut ui), Vec::<String>::new());
    ui.dispatch(&Event::ImeCommit("か".into()));
    assert!(!changes(&mut ui).is_empty());
}

/// Undo restores the caret's affinity: at the end of a soft-wrapped
/// first line (End), type, then undo: the caret is back at that line's
/// end, not at the next line's start.
#[test]
fn undo_restores_caret_affinity() {
    let mut ui = focused_input("aaaa bbbb cccc", 60.0, true);
    key(&mut ui, Key::Unknown, Some("a"), meta());
    key(
        &mut ui,
        Key::Left,
        None,
        Mods {
            meta: true,
            ..Mods::default()
        },
    );
    key(&mut ui, Key::End, None, Mods::default());
    ui.render(Size::new(800.0, 600.0));
    let caret = |ui: &Ui| ui.inputs.get(0).unwrap().editor.caret_rect(1.0).unwrap();
    let before = caret(&ui);
    ui.dispatch(&Event::KeyDown(KeyInput {
        key: Key::Unknown,
        text: Some("x".into()),
        char: Some("x".into()),
        mods: Mods::default(),
        ..KeyInput::default()
    }));
    key(&mut ui, Key::Unknown, Some("z"), meta());
    ui.render(Size::new(800.0, 600.0));
    assert_eq!(ui.inputs.text(0), "aaaa bbbb cccc");
    let after = caret(&ui);
    assert_eq!((after.0, after.1), (before.0, before.1), "caret line and x");
    assert!(
        before.1 == 0.0,
        "the caret starts on the first line: {before:?}"
    );
}

/// Finishing a composition commits its text and notifies JS once: on
/// ImeDone, and when focus leaves the input.
#[test]
fn finishing_a_composition_emits_one_change() {
    let changes = |ui: &mut Ui| -> Vec<String> {
        ui.take_events()
            .into_iter()
            .filter(|e| e.kind == out_kind::CHANGE)
            .map(|e| e.text)
            .collect()
    };
    let mut ui = focused_input("", 300.0, false);
    changes(&mut ui);
    ui.dispatch(&Event::ImePreedit {
        text: "かな".into(),
        cursor: Some((6, 6)),
    });
    assert_eq!(changes(&mut ui), Vec::<String>::new(), "a preedit alone");
    ui.dispatch(&Event::ImeDone);
    assert_eq!(changes(&mut ui), ["かな"]);
    ui.dispatch(&Event::ImeDone);
    assert_eq!(changes(&mut ui), Vec::<String>::new(), "no duplicate");

    let mut ui = focused_input("", 300.0, false);
    changes(&mut ui);
    ui.dispatch(&Event::ImePreedit {
        text: "x".into(),
        cursor: Some((1, 1)),
    });
    ui.dispatch(&Event::PointerDown {
        x: 500.0,
        y: 500.0,
        button: Button::Primary,
        mods: Mods::default(),
    });
    assert_eq!(ui.focused(), None);
    assert_eq!(changes(&mut ui), ["x"]);
}

/// A span's family resolves to a font once, when the paragraph is
/// applied (tests run on the pinned fonts, so named families): the
/// Hebrew-family span gets that face, a color change
/// keeps the resolution and does not reshape, and a family change
/// resolves again and reshapes.
#[test]
fn span_family_resolves_when_applied() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    let mono = t.family("Noto Sans Hebrew");
    let spans = [
        TextSpan::default(),
        TextSpan {
            start: 5,
            family: mono,
            ..TextSpan::default()
        },
    ];
    t.create(0, NodeKind::Text)
        .paragraph(0, "plain code", &spans)
        .place(NIL, 0, NIL);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    let p = ui.host.paragraph(NodeId(0)).unwrap().clone();
    let want = ui.text.font("Noto Sans Hebrew", 400, false);
    assert_eq!(p.fonts.len(), 2);
    assert_eq!(p.fonts[1], want);
    assert_ne!(
        p.fonts[0], p.fonts[1],
        "the base span keeps the default family"
    );
    assert_eq!(ui.host.family_name(p.spans[1].family), "Noto Sans Hebrew");

    // Color only: same fonts, no reshape.
    let shapes = ui.text.shapes;
    let mut t = Transaction::new(2);
    let mono = t.family("Noto Sans Hebrew");
    let recolored = [
        TextSpan {
            color: 0xFF00_00FF,
            ..TextSpan::default()
        },
        TextSpan {
            start: 5,
            family: mono,
            ..TextSpan::default()
        },
    ];
    t.paragraph(0, "plain code", &recolored);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    assert_eq!(ui.text.shapes, shapes);
    assert_eq!(ui.host.paragraph(NodeId(0)).unwrap().fonts, p.fonts);

    // Family change: resolves again, reshapes.
    let mut t = Transaction::new(3);
    let serif = t.family("Noto Sans Arabic");
    let refamily = [
        TextSpan::default(),
        TextSpan {
            start: 5,
            family: serif,
            ..TextSpan::default()
        },
    ];
    t.paragraph(0, "plain code", &refamily);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    assert!(ui.text.shapes > shapes);
    let fonts = &ui.host.paragraph(NodeId(0)).unwrap().fonts;
    assert_eq!(fonts[1], ui.text.font("Noto Sans Arabic", 400, false));
}

/// A pointer event on a text node carries the span under the pointer
/// (key bits 16+, span + 1), found from the placements; 0 past the text.
#[test]
fn text_pointer_events_carry_the_span() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    let spans = [
        TextSpan::default(),
        TextSpan {
            start: 4,
            weight: 700,
            ..TextSpan::default()
        },
        TextSpan {
            start: 8,
            ..TextSpan::default()
        },
    ];
    // A box wider than the text: past its end is inside the node.
    let wide = taffy::Style {
        size: taffy::Size {
            width: taffy::Dimension::length(300.0),
            height: taffy::Dimension::auto(),
        },
        ..taffy::Style::default()
    };
    t.create(0, NodeKind::Text)
        .layout(0, &wide)
        .paragraph(0, "tap here bold", &spans)
        .interaction(0, mask::POINTER_DOWN, false)
        .place(NIL, 0, NIL);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    let p = ui.text_layout(NodeId(0)).unwrap().clone();
    let line = &p.lines[0];
    let y = line.top + line.height * 0.5;
    let mut down = |x: f32| -> u32 {
        ui.take_events();
        ui.dispatch(&Event::PointerDown {
            x,
            y,
            button: Button::Primary,
            mods: Mods::default(),
        });
        ui.take_events()
            .iter()
            .find(|e| e.kind == out_kind::POINTER_DOWN)
            .map_or(u32::MAX, |e| e.key >> 16)
    };
    for v in p.visual_clusters(0) {
        let want = match v.text.start {
            0..4 => 1,
            4..8 => 2,
            _ => 3,
        };
        assert_eq!(down((v.left + v.right) * 0.5), want, "cluster {:?}", v.text);
    }
    assert_eq!(down(line.x + line.advance + 5.0), 0, "past the text");

    // Span events carry the paragraph revision (paragraph ops applied,
    // wrapping at 2^32), so JS routes a span by the table it came from. Every
    // paragraph op counts, an unchanged one too.
    let revision = |ui: &mut Ui| -> u32 {
        ui.take_events();
        ui.dispatch(&Event::PointerDown {
            x: line.x + 1.0,
            y,
            button: Button::Primary,
            mods: Mods::default(),
        });
        let e = ui.take_events();
        let e = e.iter().find(|e| e.kind == out_kind::POINTER_DOWN).unwrap();
        assert_eq!(e.key >> 16, 1);
        e.revision
    };
    assert_eq!(revision(&mut ui), 1);
    let mut t = Transaction::new(2);
    t.paragraph(0, "tap here bold", &spans);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    assert_eq!(revision(&mut ui), 2);
    let encoded = crate::events::encode_events(&[crate::events::UiEvent {
        revision: 0x0102_0304,
        ..crate::events::UiEvent::new(out_kind::POINTER_DOWN, 0)
    }]);
    // After the count: 28 bytes, then the revision (u32), then the text
    // length.
    assert_eq!(encoded.len(), 4 + 36);
    assert_eq!(encoded[4 + 28..4 + 32], 0x0102_0304u32.to_le_bytes());
}

/// Pointer events carry `span + 1` in 16 key bits: a paragraph holds at
/// most 65,535 spans (S3C-11), and the last one's event is 65,535.
#[test]
fn span_count_fits_the_event_key() {
    let text = "a".repeat(crate::executor::MAX_SPANS + 1);
    let spans: Vec<TextSpan> = (0..=crate::executor::MAX_SPANS as u32)
        .map(|start| TextSpan {
            start,
            ..TextSpan::default()
        })
        .collect();
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::Text)
        .paragraph(0, text.clone(), &spans)
        .interaction(0, mask::POINTER_DOWN, false)
        .place(NIL, 0, NIL);
    assert!(ui.apply_txn(&t).is_err(), "65,536 spans");
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::Text)
        .paragraph(0, text, &spans[..crate::executor::MAX_SPANS])
        .interaction(0, mask::POINTER_DOWN, false)
        .place(NIL, 0, NIL);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    let p = ui.text_layout(NodeId(0)).unwrap();
    // The text wraps: the last cluster ends the last line.
    let n = p.lines.len() - 1;
    let line = &p.lines[n];
    let last = p.visual_clusters(n).last().unwrap().clone();
    assert_eq!(last.text.start as usize, crate::executor::MAX_SPANS);
    let (x, y) = ((last.left + last.right) * 0.5, line.top + line.height * 0.5);
    ui.dispatch(&Event::PointerDown {
        x,
        y,
        button: Button::Primary,
        mods: Mods::default(),
    });
    let events = ui.take_events();
    let e = events
        .iter()
        .find(|e| e.kind == out_kind::POINTER_DOWN)
        .unwrap();
    assert_eq!(e.key >> 16, 0xFFFF);
}

/// Unknown span flag bits reject the transaction (S3C-12): only italic,
/// underline, and line-through exist.
#[test]
fn unknown_span_flag_bits_reject() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::Text).append(NIL, 0);
    ui.apply_txn(&t).unwrap();
    let seq = ui.seq;
    let mut t = Transaction::new(2);
    let span = TextSpan {
        color: 0xDEAD_BEEF,
        ..TextSpan::default()
    };
    t.paragraph(0, "x", &[span]);
    let buf = wire::encode(&t);
    // Span row: start u32, size f32, color u32, weight u16, then flags.
    let at = buf
        .windows(4)
        .position(|w| w == 0xDEAD_BEEFu32.to_le_bytes())
        .unwrap()
        + 6;
    assert_eq!(buf[at], 0);
    let mut ok = buf.clone();
    ok[at] = wire::span_flag::ALL;
    for bit in [1u8 << 6, 1 << 7] {
        let mut bad = buf.clone();
        bad[at] |= bit;
        assert!(ui.apply(&bad).is_err(), "bit {bit:#x}");
        assert_eq!(ui.seq, seq);
    }
    ui.apply(&ok).unwrap();
}

/// Under a plain column (10): a selectable View (0) with "Hello world"
/// (1) and, inside a plain View (2), "Second line" (3); below it, outside
/// the domain, "Outside" (5).
fn selection_ui() -> Ui {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    let column = taffy::Style {
        flex_direction: taffy::FlexDirection::Column,
        size: taffy::Size {
            width: taffy::Dimension::length(300.0),
            height: taffy::Dimension::auto(),
        },
        ..taffy::Style::default()
    };
    // A plain column root holding the selectable View and, below it,
    // text outside any domain.
    t.create(10, NodeKind::View)
        .layout(10, &column)
        .place(NIL, 10, NIL);
    t.create(0, NodeKind::View)
        .layout(0, &column)
        .interaction_flags(0, 0, false, true)
        .place(10, 0, NIL);
    t.create(1, NodeKind::Text)
        .text(1, "Hello world", 16.0, 0xFFFF_FFFF)
        .place(0, 1, NIL);
    t.create(2, NodeKind::View).place(0, 2, NIL);
    t.create(3, NodeKind::Text)
        .text(3, "Second line", 16.0, 0xFFFF_FFFF)
        .place(2, 3, NIL);
    t.create(5, NodeKind::Text)
        .text(5, "Outside", 16.0, 0xFFFF_FFFF)
        .place(10, 5, NIL);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    ui
}

/// The window point of byte `offset` of text node `id` (mid-line).
fn text_point(ui: &Ui, id: u32, offset: u32) -> (f32, f32) {
    let p = ui.text_layout(NodeId(id)).unwrap();
    let c = p.caret(offset);
    let data = ui.layouts.data(NodeId(id));
    let q = ui
        .node_to_window(NodeId(id))
        .apply(craie_core::geom::Point::new(
            data.content[0] + c.x,
            data.content[1] + c.top + c.height * 0.5,
        ));
    (q.x, q.y)
}

fn drag(ui: &mut Ui, from: (f32, f32), to: (f32, f32)) {
    ui.dispatch(&Event::PointerDown {
        x: from.0,
        y: from.1,
        button: Button::Primary,
        mods: Mods::default(),
    });
    ui.dispatch(&Event::PointerMove { x: to.0, y: to.1 });
    ui.dispatch(&Event::PointerUp {
        x: to.0,
        y: to.1,
        button: Button::Primary,
    });
}

/// Dragging selects across paragraphs in tree order (either direction);
/// copy yields one line per paragraph; Cmd+A selects the domain; a press
/// outside the domain clears; only the paragraphs whose highlight
/// changed rebuild their chunk.
#[test]
fn selection_spans_paragraphs_in_tree_order() {
    let mut ui = selection_ui();
    let (a, b) = (text_point(&ui, 1, 6), text_point(&ui, 3, 6));
    drag(&mut ui, a, b);
    assert_eq!(
        ui.selection_ranges(),
        [(NodeId(1), 6..11), (NodeId(3), 0..6)]
    );
    assert_eq!(ui.selected_text(), "world\nSecond");
    drag(&mut ui, b, a);
    assert_eq!(ui.selected_text(), "world\nSecond", "backwards");

    let before = ui.counters();
    ui.render(Size::new(400.0, 300.0));
    let built = ui.counters().since(&before).chunks_built;
    assert!(built <= 2, "only highlighted paragraphs rebuild: {built}");
    // The highlight is drawn in the selected paragraphs' chunks (text
    // chunks have no other rects here).
    let rects = |ui: &Ui, id: u32| ui.scene().chunk(id).map_or(0, |c| c.rects.len());
    assert!(rects(&ui, 1) > 0 && rects(&ui, 3) > 0);
    assert_eq!(rects(&ui, 5), 0);

    let meta = Mods::from_bits(Mods::COMMAND);
    for ch in ["a", "c"] {
        ui.dispatch(&Event::KeyDown(KeyInput {
            key: Key::Unknown,
            text: None,
            char: Some(ch.into()),
            mods: meta,
            ..KeyInput::default()
        }));
    }
    assert_eq!(
        ui.inputs.clipboard.get().as_deref(),
        Some("Hello world\nSecond line")
    );

    let outside = text_point(&ui, 5, 2);
    ui.dispatch(&Event::PointerDown {
        x: outside.0,
        y: outside.1,
        button: Button::Primary,
        mods: Mods::default(),
    });
    assert!(ui.selection_ranges().is_empty());
    assert_eq!(ui.selected_text(), "");
    ui.render(Size::new(400.0, 300.0));
    assert_eq!((rects(&ui, 1), rects(&ui, 3)), (0, 0), "highlight gone");
}

/// #28 review: a selection running past a clamped paragraph stops at
/// its ellipsis's cut: hidden text is neither highlighted nor copied
/// (PR28-06).
#[test]
fn selection_stops_at_the_line_limit() {
    use crate::selection::{TextPoint, TextSelection};
    let mut ui = Ui::new(1.0);
    ui.text = craie_text::TextEngine::with_source(Box::new(craie_text::fonts::pinned()));
    let column = taffy::Style {
        flex_direction: taffy::FlexDirection::Column,
        size: taffy::Size {
            width: taffy::Dimension::length(100.0),
            height: taffy::Dimension::auto(),
        },
        ..Default::default()
    };
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .layout(0, &column)
        .interaction_flags(0, 0, false, true)
        .append(NIL, 0);
    let text = "The quick brown fox jumps over the lazy dog";
    t.create(1, NodeKind::Text)
        .layout(1, &column)
        .text(1, text, 16.0, 0xffff_ffff)
        .lines(1, 1)
        .append(0, 1);
    t.create(2, NodeKind::Text)
        .text(2, "Next paragraph", 16.0, 0xffff_ffff)
        .append(0, 2);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(300.0, 200.0));
    let cut = ui.text_layout(NodeId(1)).unwrap().visible_end;
    assert!(cut < text.len() as u32);
    ui.set_text_selection(Some(TextSelection {
        domain: NodeId(0),
        anchor: TextPoint {
            node: NodeId(1),
            offset: 0,
        },
        focus: TextPoint {
            node: NodeId(2),
            offset: 4,
        },
    }));
    assert_eq!(ui.selection_ranges()[0], (NodeId(1), 0..cut));
    assert_eq!(
        ui.selected_text(),
        format!("{}\nNext", &text[..cut as usize])
    );
}

/// The highlight follows the tree and the text (S3C-01): a paragraph
/// inserted between the endpoints is highlighted whole; one that grows
/// is highlighted to its new end.
#[test]
fn selection_highlight_follows_tree_and_text_changes() {
    let mut ui = selection_ui();
    let (a, b) = (text_point(&ui, 1, 6), text_point(&ui, 3, 6));
    drag(&mut ui, a, b);
    ui.render(Size::new(400.0, 300.0));
    let rects = |ui: &Ui, id: u32| ui.scene().chunk(id).map_or(0, |c| c.rects.len());
    let mut t = Transaction::new(2);
    t.create(4, NodeKind::Text)
        .text(4, "Middle", 16.0, 0xFFFF_FFFF)
        .place(0, 4, 2);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    assert_eq!(
        ui.selection_ranges(),
        [(NodeId(1), 6..11), (NodeId(4), 0..6), (NodeId(3), 0..6)]
    );
    assert_eq!(rects(&ui, 4), 1, "the inserted paragraph is highlighted");
    // Growth to two lines: the highlight grows with it.
    let long = "Middle grown past the width of its box, onto a second line";
    let mut t = Transaction::new(3);
    t.text(4, long, 16.0, 0xFFFF_FFFF);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    assert_eq!(ui.selection_ranges()[1], (NodeId(4), 0..long.len() as u32));
    assert_eq!(rects(&ui, 4), 2, "one highlight rect per line");
    // The focus text hidden: the selection is dropped.
    let hidden = taffy::Style {
        display: taffy::Display::None,
        ..taffy::Style::default()
    };
    let mut t = Transaction::new(4);
    t.layout(2, &hidden);
    ui.apply_txn(&t).unwrap();
    assert_eq!(ui.text_selection(), None);
    ui.render(Size::new(400.0, 300.0));
    assert_eq!((rects(&ui, 1), rects(&ui, 4)), (0, 0), "highlight gone");
}

/// A selection belongs to the nodes it was made on (S3C-02): removing an
/// endpoint, reusing its id, or making the domain not selectable drops
/// it and its highlight.
#[test]
fn selection_drops_with_its_nodes() {
    let size = Size::new(400.0, 300.0);
    let rects = |ui: &Ui, id: u32| ui.scene().chunk(id).map_or(0, |c| c.rects.len());
    // Remove the focus text and reuse its id in one transaction.
    let mut ui = selection_ui();
    let (a, b) = (text_point(&ui, 1, 6), text_point(&ui, 3, 6));
    drag(&mut ui, a, b);
    ui.render(size);
    let mut t = Transaction::new(2);
    t.remove(3)
        .create(3, NodeKind::Text)
        .text(3, "Second line", 16.0, 0xFFFF_FFFF)
        .place(2, 3, NIL);
    ui.apply_txn(&t).unwrap();
    assert_eq!(ui.text_selection(), None);
    assert_eq!(ui.selected_text(), "");
    ui.render(size);
    assert_eq!((rects(&ui, 1), rects(&ui, 3)), (0, 0), "highlight gone");
    // The domain stops being selectable.
    let mut ui = selection_ui();
    drag(&mut ui, a, b);
    ui.render(size);
    let mut t = Transaction::new(2);
    t.interaction_flags(0, 0, false, false);
    ui.apply_txn(&t).unwrap();
    assert_eq!(ui.text_selection(), None);
    ui.render(size);
    assert_eq!((rects(&ui, 1), rects(&ui, 3)), (0, 0), "highlight gone");
    // An endpoint moves out of the domain.
    let mut ui = selection_ui();
    drag(&mut ui, a, b);
    let mut t = Transaction::new(2);
    t.place(10, 3, NIL);
    ui.apply_txn(&t).unwrap();
    assert_eq!(ui.text_selection(), None);
}

/// The selection position uses the hit text, else the nearest text in
/// both axes (S3C-03): side-by-side texts share their y range.
#[test]
fn selection_position_uses_both_axes() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    let row = taffy::Style {
        flex_direction: taffy::FlexDirection::Row,
        size: taffy::Size {
            width: taffy::Dimension::length(300.0),
            height: taffy::Dimension::auto(),
        },
        gap: taffy::Size {
            width: taffy::LengthPercentage::length(40.0),
            height: taffy::LengthPercentage::length(0.0),
        },
        ..taffy::Style::default()
    };
    t.create(0, NodeKind::View)
        .layout(0, &row)
        .interaction_flags(0, 0, false, true)
        .place(NIL, 0, NIL);
    t.create(1, NodeKind::Text)
        .text(1, "Left", 16.0, 0xFFFF_FFFF)
        .place(0, 1, NIL);
    t.create(2, NodeKind::Text)
        .text(2, "Right", 16.0, 0xFFFF_FFFF)
        .place(0, 2, NIL);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    let (a, b) = (text_point(&ui, 1, 0), text_point(&ui, 2, 2));
    drag(&mut ui, a, b);
    assert_eq!(ui.selected_text(), "Left\nRi");
    // Past the right text's end, in the row's empty space: the nearest
    // text is the right one.
    let end = text_point(&ui, 2, 5);
    drag(&mut ui, a, (end.0 + 30.0, end.1));
    assert_eq!(ui.selected_text(), "Left\nRight");
    // In the gap, nearer the left text's end.
    let gap = text_point(&ui, 1, 4);
    let from = text_point(&ui, 2, 5);
    drag(&mut ui, from, (gap.0 + 5.0, gap.1));
    assert_eq!(ui.selection_ranges(), [(NodeId(2), 0..5)]);
    // Over another text's box, the hit (topmost) text wins.
    let over = taffy::Style {
        position: taffy::Position::Absolute,
        inset: taffy::Rect {
            left: taffy::LengthPercentageAuto::length(0.0),
            top: taffy::LengthPercentageAuto::length(0.0),
            right: taffy::LengthPercentageAuto::auto(),
            bottom: taffy::LengthPercentageAuto::auto(),
        },
        ..taffy::Style::default()
    };
    let mut t = Transaction::new(2);
    t.create(3, NodeKind::Text)
        .text(3, "Over", 16.0, 0xFFFF_FFFF)
        .layout(3, &over)
        .place(0, 3, NIL);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    let (from, to) = (text_point(&ui, 2, 0), text_point(&ui, 3, 2));
    assert_eq!(ui.hit_test(to.0, to.1), Some(NodeId(3)));
    drag(&mut ui, from, to);
    assert_eq!(ui.selected_text(), "Right\nOv");
}

/// The domain must be shown (S3C-16): hiding or detaching an ancestor
/// above it drops the selection.
#[test]
fn selection_drops_when_an_ancestor_hides_or_detaches() {
    let hidden = taffy::Style {
        display: taffy::Display::None,
        ..taffy::Style::default()
    };
    for detach in [false, true] {
        let mut ui = selection_ui();
        let (a, b) = (text_point(&ui, 1, 6), text_point(&ui, 3, 6));
        drag(&mut ui, a, b);
        assert_eq!(ui.selected_text(), "world\nSecond");
        let mut t = Transaction::new(2);
        if detach {
            t.detach(10);
        } else {
            t.layout(10, &hidden);
        }
        ui.apply_txn(&t).unwrap();
        assert_eq!(ui.text_selection(), None, "detach {detach}");
        assert_eq!(ui.selected_text(), "");
    }
}

/// A primary press in an input clears the read-only selection (S3C-17).
#[test]
fn selection_clears_on_a_press_in_an_input() {
    let mut ui = selection_ui();
    let mut t = Transaction::new(2);
    let boxed = taffy::Style {
        size: taffy::Size {
            width: taffy::Dimension::length(200.0),
            height: taffy::Dimension::length(24.0),
        },
        ..taffy::Style::default()
    };
    t.create(6, NodeKind::Input)
        .layout(6, &boxed)
        .place(10, 6, NIL);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    let (a, b) = (text_point(&ui, 1, 0), text_point(&ui, 1, 5));
    drag(&mut ui, a, b);
    assert_eq!(ui.selected_text(), "Hello");
    let r = ui.layouts.data(NodeId(6)).rect;
    let at = ui
        .node_to_window(NodeId(6))
        .apply(craie_core::geom::Point::new(
            r.size.width * 0.5,
            r.size.height * 0.5,
        ));
    assert_eq!(ui.hit_test(at.x, at.y), Some(NodeId(6)));
    ui.dispatch(&Event::PointerDown {
        x: at.x,
        y: at.y,
        button: Button::Primary,
        mods: Mods::default(),
    });
    assert_eq!(ui.text_selection(), None);
    assert_eq!(ui.focus, Some(NodeId(6)));
}

/// Copy keeps empty paragraphs of the interval as empty lines (S3C-18);
/// highlights skip them.
#[test]
fn selection_copy_keeps_empty_paragraphs() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .interaction_flags(0, 0, false, true)
        .place(NIL, 0, NIL);
    for (id, text) in [(1, "A"), (2, ""), (3, "B")] {
        t.create(id, NodeKind::Text)
            .text(id, text, 16.0, 0xFFFF_FFFF)
            .place(0, id, NIL);
    }
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    ui.set_text_selection(Some(crate::selection::TextSelection {
        domain: NodeId(0),
        anchor: crate::selection::TextPoint {
            node: NodeId(1),
            offset: 0,
        },
        focus: crate::selection::TextPoint {
            node: NodeId(3),
            offset: 1,
        },
    }));
    assert_eq!(ui.selected_text(), "A\n\nB");
    assert_eq!(
        ui.selection_ranges(),
        [(NodeId(1), 0..1), (NodeId(3), 0..1)]
    );
}

/// The nearest-text distance is the true window-space distance to the
/// transformed content box (S3C-19): under shear it matches a dense
/// sampling of the box's edges.
#[test]
fn selection_distance_is_exact_under_shear() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    let boxed = taffy::Style {
        size: taffy::Size {
            width: taffy::Dimension::length(100.0),
            height: taffy::Dimension::length(40.0),
        },
        ..taffy::Style::default()
    };
    t.create(0, NodeKind::Text)
        .layout(0, &boxed)
        .text(0, "sheared", 16.0, 0xFFFF_FFFF)
        .transform(0, craie_core::geom::Affine([1.0, 0.0, 1.0, 1.0, 0.0, 0.0]))
        .place(NIL, 0, NIL);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    let m = ui.node_to_window(NodeId(0));
    let edge = |u: f32, v: f32| m.apply(craie_core::geom::Point::new(u, v));
    let sampled = |x: f32, y: f32| {
        let mut best = f32::INFINITY;
        for k in 0..=4000 {
            let s = k as f32 / 4000.0;
            for p in [
                edge(s * 100.0, 0.0),
                edge(s * 100.0, 40.0),
                edge(0.0, s * 40.0),
                edge(100.0, s * 40.0),
            ] {
                best = best.min((p.x - x).hypot(p.y - y));
            }
        }
        best
    };
    let c = edge(50.0, 20.0);
    for (dx, dy) in [
        (-90.0, 0.0),
        (90.0, 0.0),
        (0.0, -35.0),
        (-60.0, 30.0),
        (70.0, -40.0),
    ] {
        let (x, y) = (c.x + dx, c.y + dy);
        let (ours, want) = (ui.window_distance(NodeId(0), x, y), sampled(x, y));
        assert!((ours - want).abs() < 0.05, "({dx}, {dy}): {ours} vs {want}");
    }
    assert_eq!(ui.window_distance(NodeId(0), c.x, c.y), 0.0);
}

/// A reused slot resolves its paragraph's fonts afresh (S3C-14): a Text
/// created where one with another family was removed lays out as a clean
/// build does, also with a default (empty) paragraph.
#[test]
fn reused_text_slot_resolves_fonts_afresh() {
    let make = |t: &mut Transaction<'static>| {
        t.create(0, NodeKind::Text)
            .paragraph(0, "", &[TextSpan::default()])
            .place(NIL, 0, NIL);
    };
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    let hebrew = t.family("Noto Sans Hebrew");
    t.create(0, NodeKind::Text)
        .paragraph(
            0,
            "",
            &[TextSpan {
                family: hebrew,
                font_size: 40.0,
                ..TextSpan::default()
            }],
        )
        .place(NIL, 0, NIL);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    let mut t = Transaction::new(2);
    t.remove(0);
    make(&mut t);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    let mut clean = Ui::new(1.0);
    let mut t = Transaction::new(1);
    make(&mut t);
    clean.apply_txn(&t).unwrap();
    clean.render(Size::new(400.0, 300.0));
    // Font ids are per engine: each is its own engine's default.
    let fonts = |ui: &Ui| ui.host.paragraph(NodeId(0)).unwrap().fonts.clone();
    assert_eq!(fonts(&ui), [ui.text.font("", 400, false)]);
    assert_eq!(fonts(&clean), [clean.text.font("", 400, false)]);
    assert_eq!(
        ui.layouts.data(NodeId(0)).rect,
        clean.layouts.data(NodeId(0)).rect
    );
}

/// A selected paragraph that shrinks keeps a valid selection (clamped
/// onto its new text).
#[test]
fn selection_survives_a_shrinking_paragraph() {
    let mut ui = selection_ui();
    let (a, b) = (text_point(&ui, 1, 6), text_point(&ui, 1, 11));
    drag(&mut ui, a, b);
    assert_eq!(ui.selected_text(), "world");
    let mut t = Transaction::new(2);
    t.text(1, "Hé", 16.0, 0xFFFF_FFFF);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    assert_eq!(ui.selected_text(), "");
    let (a, b) = (text_point(&ui, 1, 0), text_point(&ui, 1, 3));
    drag(&mut ui, a, b);
    assert_eq!(ui.selected_text(), "Hé");
}

/// Span zero's alignment places the lines in the text node's content
/// box; changing it alone lays the paragraph out again. Tabular and the
/// alignment cross the wire in the span row's second flag byte, which
/// is strict.
#[test]
fn text_aligns_in_its_box() {
    use craie_text::paragraph::Align;
    let mut ui = Ui::new(1.0);
    ui.text = craie_text::TextEngine::with_source(Box::new(craie_text::fonts::pinned()));
    let span = |align| TextSpan {
        font_size: 16.0,
        align,
        tabular: true,
        ..TextSpan::default()
    };
    let mut style = taffy::Style::default();
    style.size.width = taffy::Dimension::length(200.0);
    let mut t = Transaction::new(1);
    t.create(1, NodeKind::Text)
        .layout(1, &style)
        .paragraph(1, "Hi", &[span(Align::Center)])
        .append(u32::MAX, 1);
    let buf = wire::encode(&t);
    assert_eq!(wire::decode(&buf).unwrap().mutations, t.mutations);
    assert_eq!(wire::decode(&buf).unwrap().spans[0], span(Align::Center));
    ui.apply(&buf).unwrap();
    ui.render(Size::new(400.0, 300.0));
    let line = |ui: &Ui| {
        let p = ui.text_layout(NodeId(1)).unwrap();
        let l = &p.lines[0];
        (l.x, l.advance - l.trailing)
    };
    let (x, w) = line(&ui);
    assert!(
        (x - (200.0 - w) / 2.0).abs() < 1e-3,
        "centered: {x} for {w}"
    );
    let mut t = Transaction::new(2);
    t.paragraph(1, "Hi", &[span(Align::Right)]);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    let (x, w) = line(&ui);
    assert!((x + w - 200.0).abs() < 1e-3, "right: {x} + {w}");
    // The feature byte's unknown bits are rejected.
    let mut bad = buf.clone();
    let row = bad
        .windows(4)
        .position(|w| w == 16.0f32.to_le_bytes())
        .unwrap();
    // font_size f32, color u32, weight u16, flags u8, then features.
    bad[row + 4 + 4 + 2 + 1] = 1 << 3;
    assert!(wire::decode(&bad).is_err());
}

/// A line limit lays the paragraph out again with its ellipsis: one
/// line truncates in the text's box, and lifting the limit restores
/// every line. Only text nodes take one; it crosses the wire.
#[test]
fn a_line_limit_truncates_text() {
    let mut ui = Ui::new(1.0);
    ui.text = craie_text::TextEngine::with_source(Box::new(craie_text::fonts::pinned()));
    let mut style = taffy::Style::default();
    style.size.width = taffy::Dimension::length(100.0);
    let text = "The quick brown fox jumps over the lazy dog";
    let mut t = Transaction::new(1);
    t.create(1, NodeKind::Text)
        .layout(1, &style)
        .text(1, text, 16.0, 0xFFFF_FFFF)
        .lines(1, 1)
        .append(u32::MAX, 1)
        .create(2, NodeKind::View)
        .append(u32::MAX, 2);
    let buf = wire::encode(&t);
    assert_eq!(wire::decode(&buf).unwrap().mutations, t.mutations);
    ui.apply(&buf).unwrap();
    ui.render(Size::new(400.0, 300.0));
    let p = ui.text_layout(NodeId(1)).unwrap();
    assert_eq!(p.lines.len(), 1);
    assert_eq!(p.ellipsis.as_ref().unwrap().line, Some(0));
    let one = ui.layouts.rect(NodeId(1)).size.height;
    let mut t = Transaction::new(2);
    t.lines(1, 0);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    let p = ui.text_layout(NodeId(1)).unwrap();
    assert!(p.lines.len() > 1 && p.ellipsis.is_none());
    assert!(
        ui.layouts.rect(NodeId(1)).size.height > one,
        "the box grows back"
    );
    let mut t = Transaction::new(3);
    t.lines(2, 1);
    assert!(ui.apply_txn(&t).is_err());
}
