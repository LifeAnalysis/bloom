//! Daemon-free Petal package builder.
//!
//! Invokes the same pure `build_petal_package_dir` path the daemon's
//! `petals.build` IPC wraps, writing `artifacts/build-manifest.json` and the
//! content-addressed route artifacts. Used by hermetic reproduction scripts
//! where no daemon may be running.
//!
//! Usage: `cargo run -p bloom-petals --example build_petal_package -- <package-dir>`

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| usage("missing package directory argument"));
    if !std::path::Path::new(&dir).is_absolute() {
        usage("package directory must be absolute");
    }
    match bloom_petals::build_petal_package_dir(&dir) {
        Ok(prepared) => {
            println!("hash: {}", prepared.hash);
            println!("name: {}", prepared.name);
            println!("routes: {}", prepared.route_index.routes.len());
        }
        Err(error) => {
            eprintln!("build failed: {error}");
            std::process::exit(1);
        }
    }
}

fn usage(message: &str) -> ! {
    eprintln!("build_petal_package: {message}");
    eprintln!("usage: build_petal_package <absolute-package-dir>");
    std::process::exit(2);
}
