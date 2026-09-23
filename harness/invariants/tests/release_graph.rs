//! Reference libraries and harness crates enter only as dev-dependencies:
//! the release dependency graphs of the shipped crates must not contain
//! them. Parley and Taffy are production engines in this milestone; they
//! join `REFERENCE_ONLY` when the owned text and layout engines replace
//! them (ARCHITECTURE.md §4, §5).

use std::process::Command;

/// Crates that ship.
const RELEASE: &[&str] = &["craie-node", "craie-platform-winit"];

/// Crates that must never appear in a release graph.
const REFERENCE_ONLY: &[&str] = &[
    "craie-harness",
    "png",
    "resvg",
    "usvg",
    "tiny-skia",
    "harfbuzz_rs",
    "harfbuzz-sys",
    "harfbuzz",
    "proptest",
    "criterion",
];

#[test]
fn release_graphs_exclude_reference_libraries() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    for krate in RELEASE {
        let out = Command::new(&cargo)
            .current_dir(root)
            .args([
                "tree",
                "--offline",
                "-e",
                "normal,build",
                "--prefix",
                "none",
                "--format",
                "{p}",
                "-p",
                krate,
            ])
            .output()
            .expect("run cargo tree");
        assert!(
            out.status.success(),
            "cargo tree failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let tree = String::from_utf8_lossy(&out.stdout);
        let names: Vec<&str> = tree
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .collect();
        assert!(names.len() > 10, "{krate}: suspiciously small graph");
        for bad in REFERENCE_ONLY {
            assert!(
                !names.contains(bad),
                "{krate} depends on reference-only crate {bad}"
            );
        }
    }
}

/// The layer map: no winit, wgpu, fonts, Taffy, or React knowledge in
/// core; no winit anywhere but platform-winit and node.
#[test]
fn layer_map_holds() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let deps = |krate: &str| -> Vec<String> {
        let out = Command::new(&cargo)
            .current_dir(root)
            .args([
                "tree",
                "--offline",
                "-e",
                "normal",
                "--prefix",
                "none",
                "--format",
                "{p}",
                "-p",
                krate,
            ])
            .output()
            .expect("run cargo tree");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split_whitespace().next().map(str::to_string))
            .collect()
    };
    let core = deps("craie-core");
    assert_eq!(core, ["craie-core"], "craie-core must have no dependencies");
    for krate in ["craie-scene", "craie-render", "craie-text", "craie-ui"] {
        let d = deps(krate);
        for bad in ["winit", "accesskit_winit", "arboard"] {
            assert!(!d.iter().any(|n| n == bad), "{krate} depends on {bad}");
        }
    }
    for krate in ["craie-scene", "craie-text", "craie-ui"] {
        assert!(
            !deps(krate).iter().any(|n| n == "wgpu"),
            "{krate} depends on wgpu"
        );
    }
    for krate in ["craie-scene", "craie-render"] {
        let d = deps(krate);
        for bad in ["parley", "swash", "taffy", "fontique"] {
            assert!(!d.iter().any(|n| n == bad), "{krate} depends on {bad}");
        }
    }
    assert!(
        !deps("craie-text").iter().any(|n| n == "taffy"),
        "craie-text depends on taffy"
    );
}
