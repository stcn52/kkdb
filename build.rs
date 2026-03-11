/// Build script: emits `cargo:rustc-cfg=kkdb_page_size="N"` based on `KKDB_PAGE_SIZE` env var.
/// Valid values: 512, 1024, 2048, 4096 (default), 8192, 16384, 32768, 65536
fn main() {
    // Declare valid values for kkdb_page_size cfg so rustc doesn't warn on #[cfg(kkdb_page_size = "...")]
    println!("cargo:rustc-check-cfg=cfg(kkdb_page_size, values(\"512\",\"1024\",\"2048\",\"4096\",\"8192\",\"16384\",\"32768\",\"65536\"))");

    let page_size: usize = std::env::var("KKDB_PAGE_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4096);

    // Validate: must be power of 2, 512 <= size <= 65536
    if !(512..=65536).contains(&page_size) || !page_size.is_power_of_two() {
        // Hard error — fail the build so the user knows their config was invalid.
        // Silently falling back to 4096 would hide misconfiguration.
        println!(
            "cargo:error=invalid KKDB_PAGE_SIZE={} (must be a power of 2 between 512 and 65536)",
            page_size
        );
        std::process::exit(1);
    }
    println!("cargo:rustc-cfg=kkdb_page_size=\"{}\"", page_size);
    println!("cargo:rustc-env=KKDB_PAGE_SIZE={}", page_size);

    // Invalidate build if KKDB_PAGE_SIZE env changes
    println!("cargo:rerun-if-env-changed=KKDB_PAGE_SIZE");
}
