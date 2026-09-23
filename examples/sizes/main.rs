//! Structural diagnostics: prints the size and allocation character of the
//! hot representations, including every fixed per-node store.
//!
//!   cargo run --example sizes
//!
//! The point is visibility, not thresholds: if an ordinary node suddenly
//! contains Vec/Arc/Box or a pile of Options, that should be visible here.

use std::mem::{align_of, size_of};

use craie_core::span::Span;
use craie_scene::{Chunk, Color, GlyphInstance, Placement, RectInstance, WorldGpu};
use craie_text::GlyphKey;
use craie_text::parley::Layout as ParleyLayout;
use craie_ui::host::{
    BoxPaint, Host, Interaction, NodeFlags, NodeHeader, NodeId, Paragraph, Spatial,
};
use craie_ui::layout::{LayoutData, MeasuredText};
use craie_ui::mutation::Mutation;

fn row(name: &str, bytes: usize, align: usize, note: &str) {
    println!("{name:<24} {bytes:>3} B  align {align:<2} {note}");
}

fn main() {
    println!("craie structural sizes\n");

    println!("host (per-node fixed cost):");
    row("NodeId", size_of::<NodeId>(), align_of::<NodeId>(), "index");
    row(
        "NodeHeader",
        size_of::<NodeHeader>(),
        align_of::<NodeHeader>(),
        "flat record: parent + child span + kind + flags + generation",
    );
    row(
        "NodeFlags",
        size_of::<NodeFlags>(),
        align_of::<NodeFlags>(),
        "dirty bits",
    );
    row(
        "Span",
        size_of::<Span>(),
        align_of::<Span>(),
        "child list in the span pool (in the header)",
    );
    row(
        "taffy::Style",
        size_of::<taffy::Style>(),
        align_of::<taffy::Style>(),
        "layout inputs row, one per node",
    );
    row(
        "Spatial",
        size_of::<Spatial>(),
        align_of::<Spatial>(),
        "transform, opacity, scroll",
    );
    row(
        "BoxPaint",
        size_of::<BoxPaint>(),
        align_of::<BoxPaint>(),
        "fill, border, radius",
    );
    row(
        "Paragraph",
        size_of::<Paragraph>(),
        align_of::<Paragraph>(),
        "UTF-8 + span list (empty = no alloc)",
    );
    row(
        "Interaction",
        size_of::<Interaction>(),
        align_of::<Interaction>(),
        "listeners, focusable, role",
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
        "MeasuredText",
        size_of::<MeasuredText>(),
        align_of::<MeasuredText>(),
        "retained Parley layout",
    );
    row(
        "parley::Layout<PaintSlot>",
        size_of::<ParleyLayout<craie_scene::PaintSlot>>(),
        align_of::<ParleyLayout<craie_scene::PaintSlot>>(),
        "shaped text per text node",
    );

    println!("\npaint / GPU data:");
    row(
        "Color",
        size_of::<Color>(),
        align_of::<Color>(),
        "0xRRGGBBAA",
    );
    row(
        "RectInstance",
        size_of::<RectInstance>(),
        align_of::<RectInstance>(),
        "rect pool row",
    );
    row(
        "GlyphInstance",
        size_of::<GlyphInstance>(),
        align_of::<GlyphInstance>(),
        "glyph pool row",
    );
    row(
        "Placement",
        size_of::<Placement>(),
        align_of::<Placement>(),
        "per chunk: offset, record, clip",
    );
    row(
        "WorldGpu",
        size_of::<WorldGpu>(),
        align_of::<WorldGpu>(),
        "per transform record",
    );
    row(
        "Chunk",
        size_of::<Chunk>(),
        align_of::<Chunk>(),
        "per chunk: spans, segments, bounds",
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
        "Mutation (max variant)",
        size_of::<Mutation>(),
        align_of::<Mutation>(),
        "decoded mutation, borrows txn buffer",
    );

    println!("\nrepresentation check (anything above holding Vec/Box/Arc/HashMap is suspect):");
    println!(
        "  NodeHeader: {} — GlyphKey: {} — GlyphInstance: {}",
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
        if size_of::<GlyphInstance>() <= 24 {
            "compact"
        } else {
            "LARGE"
        },
    );
}
