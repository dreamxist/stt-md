fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    // screencapturekit links a Swift bridge that imports Swift Concurrency.
    // The Swift Concurrency runtime ships in /usr/lib/swift on macOS 12+, but
    // the dyld lookup uses @rpath, so the binary needs LC_RPATH entries
    // pointing at the system + Xcode toolchain directories. The crate's own
    // build.rs declares these but cargo:rustc-link-arg only applies to the
    // crate that emitted it — not consumer binaries — so we have to repeat
    // them here.
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");

    if let Ok(out) = std::process::Command::new("xcode-select").arg("-p").output() {
        if out.status.success() {
            let xcode = String::from_utf8_lossy(&out.stdout).trim().to_string();
            println!(
                "cargo:rustc-link-arg=-Wl,-rpath,{xcode}/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.5/macosx"
            );
            println!(
                "cargo:rustc-link-arg=-Wl,-rpath,{xcode}/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift/macosx"
            );
        }
    }
}
