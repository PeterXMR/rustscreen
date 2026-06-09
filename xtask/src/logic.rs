//! Pure, side-effect-free helpers extracted from the packaging shell scripts so they can be
//! unit-tested without a Mac, the Android toolchain, or signing credentials. The orchestration
//! (spawning `cargo`/`gradlew`/`codesign`/`hdiutil`) lives in `main.rs`; everything that used to
//! be `awk`/`sed`/`PlistBuddy`/shell-arg-parsing lives here as testable functions.

/// One xtask subcommand, parsed from argv. Mirrors one packaging script each.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// `build_release_apk.sh` — cargo-ndk `dist` .so + `gradlew assembleRelease`.
    BuildApk,
    /// `make_app.sh` — build the `dist` host binary and assemble `RustScreen.app`.
    MakeApp { features: Option<String> },
    /// `make_dmg.sh` — wrap an existing `.app` in a distributable DMG.
    MakeDmg { bundle: Option<String> },
    /// `sign_and_notarize.sh` — codesign (hardened runtime) + notarize + staple.
    SignNotarize { bundle: Option<String> },
    /// Print usage.
    Help,
}

/// Extract the shipped version from the workspace `Cargo.toml`'s
/// `[workspace.metadata.rustscreen]` section — the Rust replacement for make_app.sh's
/// `awk -F'"' '/^\[workspace\.metadata\.rustscreen\]/{f=1} f && /^version = /{print $2; exit}'`.
///
/// Scoped to that section (a `version = "..."` BEFORE the header is ignored), so it cannot pick up
/// an unrelated key — the exact robustness the shell comment said the old `head -n1` lacked.
pub fn workspace_version(cargo_toml: &str) -> Option<String> {
    let mut in_section = false;
    for line in cargo_toml.lines() {
        let trimmed = line.trim();
        // A table header flips us in/out of the target section, so a `version` in any other
        // table is skipped (what the old `head -n1` got wrong — see make_app.sh's awk comment).
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = trimmed == "[workspace.metadata.rustscreen]";
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            if key.trim() == "version" {
                return first_quoted(value);
            }
        }
    }
    None
}

/// Return the text between the first pair of double quotes in `s` (a TOML string value).
fn first_quoted(s: &str) -> Option<String> {
    let start = s.find('"')? + 1;
    let rest = &s[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Substitute every `__VERSION__` placeholder in an Info.plist template — the Rust replacement for
/// make_app.sh's `sed "s/__VERSION__/$VERSION/g"`.
pub fn fill_version_template(template: &str, version: &str) -> String {
    template.replace("__VERSION__", version)
}

/// Compose the output DMG filename — the Rust replacement for make_dmg.sh's
/// `DMG_PATH="$DIST_DIR/$APP_NAME-$VERSION.dmg"`.
pub fn dmg_filename(app_name: &str, version: &str) -> String {
    format!("{app_name}-{version}.dmg")
}

/// Read `CFBundleShortVersionString` out of a built bundle's Info.plist — the Rust replacement for
/// make_dmg.sh's `/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString'`.
///
/// Returns the `<string>` immediately following the `CFBundleShortVersionString` `<key>`, so a
/// different version-like value earlier in the plist (e.g. `CFBundleVersion`) is never returned.
pub fn plist_short_version(plist_xml: &str) -> Option<String> {
    let key_pos = plist_xml.find("<key>CFBundleShortVersionString</key>")?;
    let after = &plist_xml[key_pos..];
    let start = after.find("<string>")? + "<string>".len();
    let rest = &after[start..];
    let end = rest.find("</string>")?;
    Some(rest[..end].trim().to_string())
}

/// Parse argv (without the program name) into a [`Command`]. The shell scripts were four separate
/// files; here one binary dispatches on the first arg, mirroring each script's own flag handling.
pub fn parse_command(args: &[String]) -> Result<Command, String> {
    let Some(first) = args.first() else {
        return Ok(Command::Help);
    };
    let rest = &args[1..];
    match first.as_str() {
        "help" | "-h" | "--help" => Ok(Command::Help),
        "build-apk" => Ok(Command::BuildApk),
        "make-app" => Ok(Command::MakeApp {
            features: parse_features(rest)?,
        }),
        "make-dmg" => Ok(Command::MakeDmg {
            bundle: rest.first().cloned(),
        }),
        "sign-notarize" => Ok(Command::SignNotarize {
            bundle: rest.first().cloned(),
        }),
        other => Err(format!(
            "unknown command: {other:?} (run `cargo xtask help`)"
        )),
    }
}

/// Parse the optional `--features <list>` flag accepted by `make-app`.
fn parse_features(rest: &[String]) -> Result<Option<String>, String> {
    match rest {
        [] => Ok(None),
        [flag, value] if flag == "--features" => Ok(Some(value.clone())),
        [flag] if flag == "--features" => {
            Err("--features requires a value (e.g. --features live-capture,live-usb)".to_string())
        }
        _ => Err(format!("make-app: unexpected arguments: {rest:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> String {
        v.to_string()
    }

    // ---- workspace_version -------------------------------------------------

    #[test]
    fn workspace_version_reads_the_metadata_section() {
        let toml = "\
[workspace]
members = [\"a\"]

[workspace.metadata.rustscreen]
app-name = \"RustScreen\"
version = \"0.1.0\"
";
        assert_eq!(workspace_version(toml), Some(s("0.1.0")));
    }

    #[test]
    fn workspace_version_ignores_a_version_before_the_section() {
        // A `version` line in an earlier table must NOT be picked up (the bug the shell's awk
        // rewrite fixed). Only the one inside [workspace.metadata.rustscreen] counts.
        let toml = "\
[package]
version = \"9.9.9\"

[workspace.metadata.rustscreen]
version = \"0.1.0\"
";
        assert_eq!(workspace_version(toml), Some(s("0.1.0")));
    }

    #[test]
    fn workspace_version_is_none_when_the_section_is_absent() {
        let toml = "[workspace]\nmembers = []\n";
        assert_eq!(workspace_version(toml), None);
    }

    // ---- fill_version_template ---------------------------------------------

    #[test]
    fn fill_version_template_replaces_every_occurrence() {
        let tpl = "<key>CFBundleVersion</key><string>__VERSION__</string>\
<key>CFBundleShortVersionString</key><string>__VERSION__</string>";
        assert_eq!(
            fill_version_template(tpl, "1.2.3"),
            "<key>CFBundleVersion</key><string>1.2.3</string>\
<key>CFBundleShortVersionString</key><string>1.2.3</string>"
        );
    }

    #[test]
    fn fill_version_template_no_placeholder_is_unchanged() {
        assert_eq!(
            fill_version_template("no marker here", "1.2.3"),
            "no marker here"
        );
    }

    // ---- dmg_filename ------------------------------------------------------

    #[test]
    fn dmg_filename_joins_name_and_version() {
        assert_eq!(dmg_filename("RustScreen", "0.1.0"), "RustScreen-0.1.0.dmg");
    }

    // ---- plist_short_version -----------------------------------------------

    #[test]
    fn plist_short_version_picks_the_short_version_key_not_an_earlier_one() {
        // CFBundleVersion (9.9.9) appears BEFORE CFBundleShortVersionString (0.1.0); the function
        // must return the short-version value, exactly like PlistBuddy `Print :CFBundle...`.
        let plist = "\
<dict>
    <key>CFBundleVersion</key>
    <string>9.9.9</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
</dict>";
        assert_eq!(plist_short_version(plist), Some(s("0.1.0")));
    }

    #[test]
    fn plist_short_version_is_none_when_key_absent() {
        let plist = "<dict><key>CFBundleName</key><string>RustScreen</string></dict>";
        assert_eq!(plist_short_version(plist), None);
    }

    // ---- parse_command -----------------------------------------------------

    #[test]
    fn parse_build_apk() {
        assert_eq!(parse_command(&[s("build-apk")]), Ok(Command::BuildApk));
    }

    #[test]
    fn parse_make_app_without_features() {
        assert_eq!(
            parse_command(&[s("make-app")]),
            Ok(Command::MakeApp { features: None })
        );
    }

    #[test]
    fn parse_make_app_with_features() {
        assert_eq!(
            parse_command(&[s("make-app"), s("--features"), s("live-capture,live-usb")]),
            Ok(Command::MakeApp {
                features: Some(s("live-capture,live-usb"))
            })
        );
    }

    #[test]
    fn parse_make_app_features_without_value_is_an_error() {
        assert!(parse_command(&[s("make-app"), s("--features")]).is_err());
    }

    #[test]
    fn parse_make_dmg_with_optional_bundle_path() {
        assert_eq!(
            parse_command(&[s("make-dmg")]),
            Ok(Command::MakeDmg { bundle: None })
        );
        assert_eq!(
            parse_command(&[s("make-dmg"), s("/tmp/RustScreen.app")]),
            Ok(Command::MakeDmg {
                bundle: Some(s("/tmp/RustScreen.app"))
            })
        );
    }

    #[test]
    fn parse_sign_notarize() {
        assert_eq!(
            parse_command(&[s("sign-notarize")]),
            Ok(Command::SignNotarize { bundle: None })
        );
    }

    #[test]
    fn parse_help_variants() {
        assert_eq!(parse_command(&[]), Ok(Command::Help));
        assert_eq!(parse_command(&[s("help")]), Ok(Command::Help));
        assert_eq!(parse_command(&[s("-h")]), Ok(Command::Help));
        assert_eq!(parse_command(&[s("--help")]), Ok(Command::Help));
    }

    #[test]
    fn parse_unknown_command_is_an_error() {
        assert!(parse_command(&[s("frobnicate")]).is_err());
    }
}
