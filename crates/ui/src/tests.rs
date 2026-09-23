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
    let wide = ui.text_layout(NodeId(1)).unwrap().len();

    ui.invalidate_layout();
    ui.render(Size::new(300.0, 800.0));
    let narrow = ui.text_layout(NodeId(1)).unwrap().len();

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
    t.input_config(0, 16.0, 0xFFFF_FFFF, "", false);
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
    }));
    ui.dispatch(&Event::KeyDown(KeyInput {
        key: Key::Unknown,
        text: Some("i".into()),
        char: Some("i".into()),
        mods: Mods::default(),
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
    t.input_config(2, 14.0, 0xFFFF_FFFF, "type here", false);
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

/// An assistive-tech Click runs the real pointer path — listeners
/// see a down/up pair at the node's center.
#[test]
fn a11y_click_synthesizes_pointer() {
    use crate::a11y::aid;
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
    t.interaction(0, mask::POINTER_DOWN | mask::POINTER_UP, false);
    t.place(NIL, 0, NIL);
    t.seq = 1;
    let buf = wire::encode(&t);

    let mut ui = Ui::new(1.0);
    ui.apply(&buf).unwrap();
    ui.render(Size::new(400.0, 300.0));
    ui.a11y_action(&ActionRequest {
        action: Action::Click,
        target_tree: TreeId::ROOT,
        target_node: aid(NodeId(0)),
        data: None,
    });
    let kinds: Vec<u8> = ui.take_events().iter().map(|e| e.kind).collect();
    assert!(kinds.contains(&out_kind::POINTER_DOWN));
    assert!(kinds.contains(&out_kind::POINTER_UP));
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
                },
            ],
        )
        .input_config(2, 15.0, 0xFFFF_FFFF, "type", true)
        .role(2, Role::TextInput)
        .label(0, "root")
        .interaction(0, mask::POINTER_DOWN, true)
        .surface(3, 7, [1, 2, 3, 4])
        .payload(3, &payload)
        .command(2, Command::SetText("seed".into()))
        .command(0, Command::ScrollTo(1.0, 2.0))
        .command(2, Command::Focus)
        .command(2, Command::Blur)
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
