//! Structural diagnostics: prints the size and allocation character of the
//! hot representations, including every fixed per-node store.
//!
//!   cargo run --example sizes
//!
//! The point is visibility, not thresholds: if an ordinary node suddenly
//! contains Vec/Arc/Box or a pile of Options, that should be visible here.

use std::mem::{align_of, size_of};

use craie::host::{Host, NodeFlags, NodeHeader, NodeId, StyleId, TextRow, ViewRow};
use craie::layout::{EmittedText, LayoutData, MeasuredText};
use craie::scene::{Color, Instance};
use craie::text::GlyphKey;
use craie::text::parley::Layout as ParleyLayout;
use craie::wire;

fn row(name: &str, bytes: usize, align: usize, note: &str) {
    println!("{name:<24} {bytes:>3} B  align {align:<2} {note}");
}

fn main() {
    println!("craie structural sizes\n");

    println!("host (per-node fixed cost):");
    row("NodeId", size_of::<NodeId>(), align_of::<NodeId>(), "index");
    row("StyleId", size_of::<StyleId>(), align_of::<StyleId>(), "wire id");
    row(
        "NodeHeader",
        size_of::<NodeHeader>(),
        align_of::<NodeHeader>(),
        "flat record: parent + aux + style + kind/gen + flags",
    );
    row(
        "NodeFlags",
        size_of::<NodeFlags>(),
        align_of::<NodeFlags>(),
        "dirty bits",
    );
    row(
        "children slot",
        size_of::<Vec<NodeId>>(),
        align_of::<Vec<NodeId>>(),
        "Vec<NodeId> per node (empty = no alloc)",
    );
    row(
        "TextRow",
        size_of::<TextRow>(),
        align_of::<TextRow>(),
        "TEXT side-table row",
    );
    row(
        "ViewRow",
        size_of::<ViewRow>(),
        align_of::<ViewRow>(),
        "VIEW side-table row",
    );
    row("Host", size_of::<Host>(), align_of::<Host>(), "arena owner");

    println!("\nlayout (per-node fixed cost):");
    row(
        "taffy::Cache",
        size_of::<taffy::Cache>(),
        align_of::<taffy::Cache>(),
        "Taffy cache entry per node",
    );
    row(
        "taffy::Layout",
        size_of::<taffy::Layout>(),
        align_of::<taffy::Layout>(),
        "unrounded result per node",
    );
    row(
        "LayoutData",
        size_of::<LayoutData>(),
        align_of::<LayoutData>(),
        "final border box per node",
    );
    row(
        "taffy::Style",
        size_of::<taffy::Style>(),
        align_of::<taffy::Style>(),
        "per wire style id",
    );
    row(
        "MeasuredText",
        size_of::<MeasuredText>(),
        align_of::<MeasuredText>(),
        "retained Parley layout + emitted batch slot",
    );
    row(
        "parley::Layout<Color>",
        size_of::<ParleyLayout<Color>>(),
        align_of::<ParleyLayout<Color>>(),
        "shaped text per text node",
    );
    row(
        "EmittedText",
        size_of::<EmittedText>(),
        align_of::<EmittedText>(),
        "paint-ready instance batch",
    );

    println!("\npaint / GPU data:");
    row("Color", size_of::<Color>(), align_of::<Color>(), "0xRRGGBBAA");
    row(
        "Instance",
        size_of::<Instance>(),
        align_of::<Instance>(),
        "unified vertex row (quad or glyph)",
    );

    println!("\ntext cache:");
    row(
        "GlyphKey",
        size_of::<GlyphKey>(),
        align_of::<GlyphKey>(),
        "font+coords interned, size bits, quarter-px subpixel",
    );

    println!("\nwire:");
    row(
        "Op (max variant)",
        size_of::<wire::Op>(),
        align_of::<wire::Op>(),
        "decoded op, borrows txn buffer",
    );

    println!("\nrepresentation check (anything above holding Vec/Box/Arc/HashMap is suspect):");
    println!(
        "  NodeHeader: {} — GlyphKey: {} — Instance: {}",
        if size_of::<NodeHeader>() <= 24 {
            "compact"
        } else {
            "LARGE"
        },
        if size_of::<GlyphKey>() <= 24 {
            "compact"
        } else {
            "LARGE"
        },
        if size_of::<Instance>() <= 48 {
            "compact"
        } else {
            "LARGE"
        },
    );
}
