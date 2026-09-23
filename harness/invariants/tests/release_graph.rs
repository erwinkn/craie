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

/// Features each package resolves to in a build of `args`, as
/// (package name, features).
fn resolved_features(args: &[&str]) -> Vec<(String, String)> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
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
            "{p}|{f}",
        ])
        .args(args)
        .output()
        .expect("run cargo tree");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (p, f) = l.split_once('|')?;
            let name = p.split_whitespace().next()?.to_string();
            Some((name, f.trim_end_matches(" (*)").to_string()))
        })
        .collect()
}

/// Test-only features never reach a release build. The production build
/// is per package (`cargo build --release -p craie-node`, as
/// `scripts/build-addon.sh` does): there, craie-text resolves without
/// `pinned-fonts`. Negative controls: the harness, and a `--workspace`
/// build (which unifies features with the harness), resolve it on, so a
/// workspace build is not a font-free release.
#[test]
fn release_builds_exclude_test_features() {
    let text_features = |args: &[&str]| -> Vec<String> {
        resolved_features(args)
            .into_iter()
            .filter(|(p, _)| p == "craie-text")
            .map(|(_, f)| f)
            .collect()
    };
    for krate in RELEASE {
        let f = text_features(&["-p", krate]);
        assert!(!f.is_empty(), "{krate}: craie-text missing from the graph");
        assert!(
            f.iter().all(|f| !f.contains("pinned-fonts")),
            "{krate}: {f:?}"
        );
    }
    assert!(
        text_features(&["-p", "craie-harness"])
            .iter()
            .any(|f| f.contains("pinned-fonts"))
    );
    assert!(
        text_features(&["--workspace"])
            .iter()
            .any(|f| f.contains("pinned-fonts"))
    );
}
