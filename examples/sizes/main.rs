//! Structural diagnostics: prints the size and allocation character of the
//! hot representations.
//!
//!   cargo run --example sizes
//!
//! The point is visibility, not thresholds: if an ordinary node suddenly
//! contains Vec/Arc/Box or a pile of Options, that should be visible here.

use std::mem::{align_of, size_of};

use craie::host::{Host, NodeFlags, NodeHeader, NodeId, StyleId};
use craie::layout::LayoutData;
use craie::scene::{Color, GlyphInstance, QuadInstance};
use craie::text::GlyphKey;

fn row(name: &str, bytes: usize, align: usize, note: &str) {
    println!("{name:<24} {bytes:>3} B  align {align:<2} {note}");
}

fn main() {
    println!("craie structural sizes\n");

    println!("host:");
    row("NodeId", size_of::<NodeId>(), align_of::<NodeId>(), "index");
    row(
        "StyleId",
        size_of::<StyleId>(),
        align_of::<StyleId>(),
        "interned",
    );
    row(
        "NodeHeader",
        size_of::<NodeHeader>(),
        align_of::<NodeHeader>(),
        "flat record: links + aux + style + kind/gen + flags",
    );
    row(
        "NodeFlags",
        size_of::<NodeFlags>(),
        align_of::<NodeFlags>(),
        "dirty bits",
    );
    row("Host", size_of::<Host>(), align_of::<Host>(), "arena owner");

    println!("\nlayout:");
    row(
        "LayoutData",
        size_of::<LayoutData>(),
        align_of::<LayoutData>(),
        "computed border box",
    );

    println!("\npaint / GPU data:");
    row(
        "Color",
        size_of::<Color>(),
        align_of::<Color>(),
        "0xRRGGBBAA",
    );
    row(
        "QuadInstance",
        size_of::<QuadInstance>(),
        align_of::<QuadInstance>(),
        "vertex-shader row",
    );
    row(
        "GlyphInstance",
        size_of::<GlyphInstance>(),
        align_of::<GlyphInstance>(),
        "vertex-shader row",
    );

    println!("\ntext cache:");
    row(
        "GlyphKey",
        size_of::<GlyphKey>(),
        align_of::<GlyphKey>(),
        "font+coords interned, size bits, quarter-px subpixel",
    );

    println!("\nrepresentation check (anything above holding Vec/Box/Arc/HashMap is suspect):");
    println!(
        "  NodeHeader: {} — GlyphKey: {} — GlyphInstance: {}",
        if size_of::<NodeHeader>() <= 48 {
            "compact"
        } else {
            "LARGE"
        },
        if size_of::<GlyphKey>() <= 24 {
            "compact"
        } else {
            "LARGE"
        },
        if size_of::<GlyphInstance>() <= 48 {
            "compact"
        } else {
            "LARGE"
        },
    );
}
