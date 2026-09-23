//! `craie-svg in.svg out.crv [--lenient]`: imports an SVG into a Craie
//! vector asset. Unsupported features are listed on stderr and fail the
//! import unless `--lenient`.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let lenient = args.iter().any(|a| a == "--lenient");
    let paths: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let [input, output] = paths[..] else {
        eprintln!("usage: craie-svg in.svg out.crv [--lenient]");
        return ExitCode::from(2);
    };
    let svg = match std::fs::read(input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{input}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (asset, report) = match craie_svg_import::import(&svg) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{input}: {e}");
            return ExitCode::FAILURE;
        }
    };
    for u in &report.unsupported {
        eprintln!("{input}: unsupported: {u}");
    }
    if !report.unsupported.is_empty() && !lenient {
        eprintln!("{input}: not written (pass --lenient to write it anyway)");
        return ExitCode::FAILURE;
    }
    let bytes = craie_vector::asset::encode(&asset);
    if let Err(e) = std::fs::write(output, &bytes) {
        eprintln!("{output}: {e}");
        return ExitCode::FAILURE;
    }
    eprintln!(
        "{output}: {} items, {} bytes",
        asset.items.len(),
        bytes.len()
    );
    ExitCode::SUCCESS
}
