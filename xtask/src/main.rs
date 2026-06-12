//! `cargo xtask` — RustScreen's build/release automation, the Rust replacement for the four
//! `packaging/**.sh` scripts. Invoked via the alias in `.cargo/config.toml`:
//!
//! ```text
//! cargo xtask build-apk                       # = packaging/android/build_release_apk.sh
//! cargo xtask make-app [--features <list>]     # = packaging/macos/make_app.sh
//! cargo xtask make-dmg [path/to/RustScreen.app]   # = packaging/macos/make_dmg.sh
//! cargo xtask sign-notarize [path/to/...app]      # = packaging/macos/sign_and_notarize.sh
//! ```
//!
//! This is build TOOLING only — never on the glass-to-glass latency path. The pure parsing /
//! templating logic lives (and is unit-tested) in [`logic`]; this file is the orchestration that
//! spawns the same external tools the shell scripts did (`cargo`, `gradlew`, `codesign`, `ditto`,
//! `hdiutil`, `xcrun`), now with typed errors and `?`-propagation instead of `set -euo pipefail`.

mod logic;

use logic::{parse_command, Command as Sub};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::{env, fs};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let cmd = match parse_command(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("xtask: {e}");
            return ExitCode::FAILURE;
        }
    };

    let result = match cmd {
        Sub::Help => {
            print_help();
            Ok(())
        }
        Sub::BuildApk => build_apk(),
        Sub::MakeApp { features } => make_app(features.as_deref()),
        Sub::MakeDmg { bundle } => make_dmg(bundle.as_deref()),
        Sub::SignNotarize { bundle } => sign_notarize(bundle.as_deref()),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// Subcommands (one per shell script)
// ---------------------------------------------------------------------------

/// `build_release_apk.sh`: cross-compile the `dist` cdylib into jniLibs via cargo-ndk, then
/// assemble the release APK via Gradle.
fn build_apk() -> Result<(), String> {
    let root = repo_root();
    // Mirror the script's `: "${ANDROID_NDK_HOME:?...}"` precondition.
    env_required("ANDROID_NDK_HOME")
        .map_err(|_| "export ANDROID_NDK_HOME (e.g. $ANDROID_HOME/ndk/25.2.9519653)".to_string())?;

    let jni_libs = root.join("android/app/src/main/jniLibs");
    println!("==> 1/2 Building android-client .so (dist) via cargo-ndk");
    // `--features live-decode` compiles the AMediaCodec decode-to-surface adapter + the JNI entries
    // the app needs; omitting it ships a .so missing those symbols → UnsatisfiedLinkError on launch.
    // `--profile dist` is the size-optimized, stripped shipped profile.
    run(
        "cargo ndk build",
        Command::new("cargo")
            .current_dir(&root)
            .args(["ndk", "-t", "arm64-v8a", "-o"])
            .arg(&jni_libs)
            .args([
                "build",
                "--profile",
                "dist",
                "-p",
                "android-client",
                "--features",
                "live-decode",
            ]),
    )?;

    println!("==> 2/2 Assembling release APK via Gradle");
    run(
        "gradlew assembleRelease",
        Command::new("./gradlew")
            .current_dir(root.join("android"))
            .arg("assembleRelease"),
    )?;

    let apk = root.join("android/app/build/outputs/apk/release/app-release.apk");
    println!("==> Done.");
    if apk.is_file() {
        println!("    APK: {}", apk.display());
    } else {
        println!("    Expected APK at: {}", apk.display());
        println!("    (Gradle may name it app-release-unsigned.apk if signing is not configured.)");
    }
    println!(
        "    Install on a Pixel 6a in developer mode:  adb install \"{}\"",
        apk.display()
    );
    Ok(())
}

/// Features `make-app` builds with when `--features` is not given: the full live host,
/// so the bundled executable is the REAL `rustscreen` CLI. (The retired `make_app.sh`
/// bundled the 6-line version-printing `macos-host` stub — a P8 scaffold from before the
/// live pipeline existed; faithfully ported at first, retired here.)
const MAKE_APP_DEFAULT_FEATURES: &str = "live-capture,live-usb";

/// `make_app.sh`: build the `dist` `rustscreen` binary and assemble `dist/RustScreen.app`.
fn make_app(features: Option<&str>) -> Result<(), String> {
    require_macos("make-app")?;
    let root = repo_root();

    // Version lives in ONE place: the workspace metadata. Read it directly (no awk).
    let cargo_toml = read(&root.join("Cargo.toml"))?;
    let version = logic::workspace_version(&cargo_toml).unwrap_or_else(|| "0.0.0".to_string());

    // The bundle ships the real CLI (`[[bin]] rustscreen`, required-features
    // live-capture,live-usb), so those features are the default; `--features` can extend
    // them (e.g. adding live-inject). Passing a list that drops a required feature fails
    // the `--bin rustscreen` build below with cargo's own clear error.
    let features = features.unwrap_or(MAKE_APP_DEFAULT_FEATURES);
    println!("==> Building rustscreen (dist, features: {features})");
    let mut build = Command::new("cargo");
    build.current_dir(&root).args([
        "build",
        "--profile",
        "dist",
        "-p",
        "macos-host",
        "--bin",
        "rustscreen",
        "--features",
        features,
    ]);
    run("cargo build", &mut build)?;

    let bin_path = root.join("target/dist/rustscreen");
    if !bin_path.is_file() {
        return Err(format!(
            "expected binary not found at {}",
            bin_path.display()
        ));
    }

    let dist_dir = root.join("dist");
    let app_bundle = dist_dir.join("RustScreen.app");
    let pkg_dir = root.join("packaging/macos");
    println!("==> Assembling {}", app_bundle.display());

    rm_rf(&app_bundle)?;
    let macos_dir = app_bundle.join("Contents/MacOS");
    let resources_dir = app_bundle.join("Contents/Resources");
    mkdirs(&macos_dir)?;
    mkdirs(&resources_dir)?;

    // Info.plist with __VERSION__ substituted (no sed).
    let template = read(&pkg_dir.join("Info.plist"))?;
    write(
        &app_bundle.join("Contents/Info.plist"),
        &logic::fill_version_template(&template, &version),
    )?;

    // The executable (copy + chmod +x). Name must match CFBundleExecutable in Info.plist.
    let dest_bin = macos_dir.join("rustscreen");
    fs::copy(&bin_path, &dest_bin)
        .map_err(|e| format!("copy binary → {} failed: {e}", dest_bin.display()))?;
    set_executable(&dest_bin)?;

    // PkgInfo (optional but conventional).
    write(&app_bundle.join("Contents/PkgInfo"), "APPL????")?;

    // AppIcon: optional. Copy it in if present, else note its absence (matches the script).
    let icon_src = pkg_dir.join("AppIcon.icns");
    if icon_src.is_file() {
        let icon_dest = resources_dir.join("AppIcon.icns");
        fs::copy(&icon_src, &icon_dest).map_err(|e| format!("copy AppIcon.icns failed: {e}"))?;
    } else {
        println!("    (no packaging/macos/AppIcon.icns — bundle has no icon yet)");
    }

    println!("==> Done: {} (version {version})", app_bundle.display());
    println!("    Next: cargo xtask sign-notarize to sign + notarize,");
    println!("          then cargo xtask make-dmg to produce a distributable DMG.");
    Ok(())
}

/// `make_dmg.sh`: wrap an existing `.app` in a distributable UDZO DMG with an /Applications symlink.
fn make_dmg(bundle: Option<&str>) -> Result<(), String> {
    require_macos("make-dmg")?;
    let root = repo_root();
    let dist_dir = root.join("dist");
    let app_bundle = bundle
        .map(PathBuf::from)
        .unwrap_or_else(|| dist_dir.join("RustScreen.app"));
    if !app_bundle.is_dir() {
        return Err(format!(
            "app bundle not found: {} (run `cargo xtask make-app` first)",
            app_bundle.display()
        ));
    }

    // Version from the built bundle's Info.plist (no PlistBuddy).
    let plist = read(&app_bundle.join("Contents/Info.plist"))?;
    let version = logic::plist_short_version(&plist).unwrap_or_else(|| "0.0.0".to_string());
    let dmg_path = dist_dir.join(logic::dmg_filename("RustScreen", &version));

    println!("==> Staging DMG contents");
    // A per-process staging dir under the system temp dir (the typed equivalent of `mktemp -d`),
    // cleaned up on the way out. process::id() keeps it unique without needing a clock/RNG.
    let stage = env::temp_dir().join(format!("rustscreen-dmg-{}", std::process::id()));
    rm_rf(&stage)?;
    mkdirs(&stage)?;

    // `cp -R bundle stage/RustScreen.app` (recursive copy; cp is a system tool like the others).
    run(
        "cp -R",
        Command::new("cp")
            .arg("-R")
            .arg(&app_bundle)
            .arg(stage.join("RustScreen.app")),
    )?;
    // `ln -s /Applications stage/Applications` — the drag-install target.
    std::os::unix::fs::symlink("/Applications", stage.join("Applications"))
        .map_err(|e| format!("symlink /Applications failed: {e}"))?;

    println!("==> Building {}", dmg_path.display());
    let _ = fs::remove_file(&dmg_path); // -f: ignore if absent
    let dmg_result = run(
        "hdiutil create",
        Command::new("hdiutil")
            .args(["create", "-volname", "RustScreen", "-srcfolder"])
            .arg(&stage)
            .args(["-ov", "-format", "UDZO"])
            .arg(&dmg_path),
    );
    rm_rf(&stage)?; // always clean up the staging dir (mirrors the script's EXIT trap)
    dmg_result?;

    println!("==> Done: {} (version {version})", dmg_path.display());
    println!("    Drag RustScreen.app to Applications to install.");
    Ok(())
}

/// `sign_and_notarize.sh`: codesign (hardened runtime) → notarize → staple → assess.
fn sign_notarize(bundle: Option<&str>) -> Result<(), String> {
    require_macos("sign-notarize")?;
    let root = repo_root();
    let app_bundle = bundle
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("dist/RustScreen.app"));
    if !app_bundle.is_dir() {
        return Err(format!(
            "app bundle not found: {} (run `cargo xtask make-app` first)",
            app_bundle.display()
        ));
    }
    let sign_identity = env_required("SIGN_IDENTITY")
        .map_err(|_| "set SIGN_IDENTITY to your Developer ID Application identity".to_string())?;
    let notary_profile = env_required("NOTARY_PROFILE")
        .map_err(|_| "set NOTARY_PROFILE to a notarytool keychain profile name".to_string())?;
    let entitlements = root.join("packaging/macos/entitlements.plist");

    println!(
        "==> Codesigning (hardened runtime) {}",
        app_bundle.display()
    );
    // --options runtime = hardened runtime; --timestamp = secure timestamp (both required for
    // notarization). --deep is intentionally omitted (Apple deprecates it for signing).
    run(
        "codesign",
        Command::new("codesign")
            .args([
                "--force",
                "--options",
                "runtime",
                "--timestamp",
                "--entitlements",
            ])
            .arg(&entitlements)
            .args(["--sign", &sign_identity])
            .arg(&app_bundle),
    )?;

    println!("==> Verifying signature");
    run(
        "codesign --verify",
        Command::new("codesign")
            .args(["--verify", "--strict", "--verbose=2"])
            .arg(&app_bundle),
    )?;

    println!("==> Zipping for notarization");
    let zip_path = root.join("dist/RustScreen-notarize.zip");
    let _ = fs::remove_file(&zip_path);
    run(
        "ditto",
        Command::new("/usr/bin/ditto")
            .args(["-c", "-k", "--keepParent"])
            .arg(&app_bundle)
            .arg(&zip_path),
    )?;

    println!("==> Submitting to Apple notary service (this can take a few minutes)");
    run(
        "notarytool submit",
        Command::new("xcrun")
            .args(["notarytool", "submit"])
            .arg(&zip_path)
            .args(["--keychain-profile", &notary_profile, "--wait"]),
    )?;

    println!("==> Stapling the notarization ticket");
    run(
        "stapler staple",
        Command::new("xcrun")
            .args(["stapler", "staple"])
            .arg(&app_bundle),
    )?;

    println!("==> Validating staple + Gatekeeper assessment");
    run(
        "stapler validate",
        Command::new("xcrun")
            .args(["stapler", "validate"])
            .arg(&app_bundle),
    )?;
    // spctl assessment is advisory in the script (it tolerates a non-zero exit) — keep that.
    if run_allow_fail(
        Command::new("spctl")
            .args(["--assess", "--type", "execute", "--verbose=4"])
            .arg(&app_bundle),
    )
    .is_err()
    {
        println!("    (spctl assessment non-zero — review output above)");
    }

    let _ = fs::remove_file(&zip_path);
    println!("==> Signed + notarized: {}", app_bundle.display());
    println!("    Next: cargo xtask make-dmg to wrap it in a distributable DMG.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Repo root = the xtask crate's parent. Resolved at compile time so it is correct regardless of
/// the caller's working directory (the shell scripts derived this from `${BASH_SOURCE[0]}`).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask crate always has a parent (the repo root)")
        .to_path_buf()
}

fn require_macos(cmd: &str) -> Result<(), String> {
    if env::consts::OS == "macos" {
        Ok(())
    } else {
        Err(format!(
            "{cmd}: must be run on macOS (host is {})",
            env::consts::OS
        ))
    }
}

fn env_required(name: &str) -> Result<String, String> {
    env::var(name).map_err(|_| format!("environment variable {name} is required"))
}

/// Run a command inheriting stdio (so the user sees cargo/gradle/codesign output live), erroring
/// with a labeled message on launch failure or a non-zero exit — the `set -e` equivalent.
fn run(label: &str, cmd: &mut Command) -> Result<(), String> {
    let status = cmd
        .status()
        .map_err(|e| format!("failed to launch {label}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{label} failed ({status})"))
    }
}

/// Like [`run`] but returns the raw status as a `Result` for callers that tolerate a non-zero exit.
fn run_allow_fail(cmd: &mut Command) -> Result<(), ()> {
    match cmd.status() {
        Ok(s) if s.success() => Ok(()),
        _ => Err(()),
    }
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("read {} failed: {e}", path.display()))
}

fn write(path: &Path, contents: &str) -> Result<(), String> {
    fs::write(path, contents).map_err(|e| format!("write {} failed: {e}", path.display()))
}

fn mkdirs(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|e| format!("mkdir -p {} failed: {e}", path.display()))
}

/// `rm -rf`: remove a directory tree, treating "already absent" as success.
fn rm_rf(path: &Path) -> Result<(), String> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("rm -rf {} failed: {e}", path.display())),
    }
}

fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)
        .map_err(|e| format!("stat {} failed: {e}", path.display()))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).map_err(|e| format!("chmod +x {} failed: {e}", path.display()))
}

fn print_help() {
    println!(
        "cargo xtask — RustScreen build/release automation (replaces packaging/**.sh)

USAGE:
    cargo xtask <COMMAND>

COMMANDS:
    build-apk                       Build the release Android APK (cargo-ndk dist .so + Gradle).
                                    Requires ANDROID_NDK_HOME.
    make-app [--features <list>]    Build the dist `rustscreen` binary and assemble
                                    dist/RustScreen.app. macOS only. Defaults to
                                    --features live-capture,live-usb (the live host);
                                    e.g. --features live-capture,live-usb,live-inject
    make-dmg [<bundle>]             Wrap an .app (default dist/RustScreen.app) in a DMG. macOS only.
    sign-notarize [<bundle>]        Codesign + notarize + staple. macOS only.
                                    Requires SIGN_IDENTITY and NOTARY_PROFILE.
    help                            Show this help.

Supersedes the former packaging/**.sh scripts (now removed)."
    );
}
