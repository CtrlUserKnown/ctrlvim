//! `cvi tool fetch-release`: a generic GitHub-release-binary installer.
//!
//! This is the piece `lsp.lua` was missing (see `wikis/lsp-mason-plan.md`):
//! a server with no `cargo`/`npm`/`pip`/`go install` path, only a GitHub
//! release archive. ctrlvim still has no built-in knowledge of *which*
//! servers need this — the user's `install` line supplies the repo, tag,
//! and exact asset filename; this just does the generic download-extract-
//! chmod dance and drops the result under ctrlvim's own tools directory
//! (see [`crate::data::tools_root`]), where [`crate::data::locate`] then
//! finds it without touching `$PATH`.
//!
//! Invoked as a hidden CLI subcommand (`cvi tool fetch-release ...`),
//! dispatched in `main.rs` before the TUI ever starts — the same way
//! `--help`/`--version` short-circuit before a terminal is touched.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Parsed `fetch-release` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchSpec {
    /// `owner/repo`.
    pub repo: String,
    /// A release tag, or `"latest"`.
    pub tag: String,
    /// The exact release asset filename to download.
    pub asset: String,
    /// Installed under `~/.local/share/ctrlvim/tools/<dest>/` — also what
    /// `lsp.lua`'s declared `name` should match, so `locate` finds it.
    pub dest: String,
    /// Leading path components stripped off every archive entry before
    /// it's written — the usual fix for an archive that wraps everything
    /// in one top-level directory.
    pub strip_components: usize,
    /// Path (relative to the extracted `dest`) to mark executable.
    pub bin: Option<String>,
}

/// Parse the arguments after `fetch-release` itself.
pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<FetchSpec, String> {
    let mut repo = None;
    let mut tag = "latest".to_string();
    let mut asset = None;
    let mut dest = None;
    let mut strip_components = 0usize;
    let mut bin = None;

    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{arg}: expected a value"));
        match arg.as_str() {
            "--repo" => repo = Some(value()?),
            "--tag" => tag = value()?,
            "--asset" => asset = Some(value()?),
            "--dest" => dest = Some(value()?),
            "--strip-components" => {
                let v = value()?;
                strip_components =
                    v.parse().map_err(|_| format!("--strip-components: not a number: '{v}'"))?;
            }
            "--bin" => bin = Some(value()?),
            other => return Err(format!("unknown argument '{other}'")),
        }
    }

    Ok(FetchSpec {
        repo: repo.ok_or("--repo is required")?,
        tag,
        asset: asset.ok_or("--asset is required")?,
        dest: dest.ok_or("--dest is required")?,
        strip_components,
        bin,
    })
}

#[derive(Debug, Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Deserialize)]
struct Release {
    assets: Vec<ReleaseAsset>,
}

/// GitHub API URL for a release — `latest`, or a specific tag.
fn release_url(repo: &str, tag: &str) -> String {
    if tag == "latest" {
        format!("https://api.github.com/repos/{repo}/releases/latest")
    } else {
        format!("https://api.github.com/repos/{repo}/releases/tags/{tag}")
    }
}

/// Pick `asset_name` out of a parsed release's assets. Pure — split out
/// from the network call so it's testable against a fixture payload.
fn find_asset_url<'a>(release: &'a Release, asset_name: &str) -> Result<&'a str, String> {
    release.assets.iter().find(|a| a.name == asset_name).map(|a| a.browser_download_url.as_str()).ok_or_else(|| {
        let available: Vec<&str> = release.assets.iter().map(|a| a.name.as_str()).collect();
        format!("asset '{asset_name}' not found in the release; available: {}", available.join(", "))
    })
}

/// Which extraction path an asset filename asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveFormat {
    TarGz,
    Zip,
    /// Neither — some tools publish the bare executable as the release
    /// asset directly (common for small Go/Rust binaries).
    Raw,
}

fn detect_format(asset_name: &str) -> ArchiveFormat {
    let lower = asset_name.to_ascii_lowercase();
    if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        ArchiveFormat::TarGz
    } else if lower.ends_with(".zip") {
        ArchiveFormat::Zip
    } else {
        ArchiveFormat::Raw
    }
}

/// Strip `n` leading path components off `path`, returning `None` if that
/// consumes the whole path (a bare top-level directory entry, once
/// stripped, has nothing left to create).
fn strip_prefix_components(path: &Path, n: usize) -> Option<PathBuf> {
    let mut comps = path.components();
    for _ in 0..n {
        comps.next()?;
    }
    let rest: PathBuf = comps.collect();
    (!rest.as_os_str().is_empty()).then_some(rest)
}

fn fetch_release(repo: &str, tag: &str) -> Result<Release, String> {
    let url = release_url(repo, tag);
    let resp = ureq::get(&url)
        .set("User-Agent", "ctrlvim")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("GET {url}: {e}"))?;
    resp.into_json::<Release>().map_err(|e| format!("parsing release JSON from {url}: {e}"))
}

fn download_to(url: &str, dest: &Path) -> Result<(), String> {
    let resp = ureq::get(url).set("User-Agent", "ctrlvim").call().map_err(|e| format!("GET {url}: {e}"))?;
    let mut reader = resp.into_reader();
    let mut file = fs::File::create(dest).map_err(|e| format!("creating {}: {e}", dest.display()))?;
    io::copy(&mut reader, &mut file).map_err(|e| format!("downloading {url}: {e}"))?;
    Ok(())
}

fn extract_tar_gz(archive_path: &Path, dest: &Path, strip: usize) -> Result<(), String> {
    let file = fs::File::open(archive_path).map_err(|e| e.to_string())?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        let Some(rel) = strip_prefix_components(&path, strip) else { continue };
        let out_path = dest.join(&rel);
        if entry.header().entry_type().is_dir() {
            fs::create_dir_all(&out_path).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        entry.unpack(&out_path).map_err(|e| format!("extracting {}: {e}", path.display()))?;
    }
    Ok(())
}

fn extract_zip(archive_path: &Path, dest: &Path, strip: usize) -> Result<(), String> {
    let file = fs::File::open(archive_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let Some(path) = entry.enclosed_name() else { continue };
        let Some(rel) = strip_prefix_components(&path, strip) else { continue };
        let out_path = dest.join(&rel);
        if entry.is_dir() {
            fs::create_dir_all(&out_path).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut out_file = fs::File::create(&out_path).map_err(|e| e.to_string())?;
        io::copy(&mut entry, &mut out_file).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = entry.unix_mode() {
                let _ = fs::set_permissions(&out_path, fs::Permissions::from_mode(mode));
            }
        }
    }
    Ok(())
}

fn extract_raw(archive_path: &Path, dest: &Path, out_name: &str) -> Result<(), String> {
    let out_path = dest.join(out_name);
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::copy(archive_path, &out_path).map_err(|e| e.to_string())?;
    Ok(())
}

/// Make `path` executable, on top of whatever the archive itself recorded —
/// belt and braces, since a `.zip` entry commonly carries no unix mode at
/// all.
#[cfg(unix)]
fn mark_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(perms.mode() | 0o111);
        let _ = fs::set_permissions(path, perms);
    }
}
#[cfg(not(unix))]
fn mark_executable(_path: &Path) {}

/// Download, extract, and (if `--bin` was given) chmod +x. Returns the
/// resolved `--bin` path, or the extraction root if none was given.
pub fn run(spec: &FetchSpec) -> Result<PathBuf, String> {
    let dest_root = crate::data::tools_root(&spec.dest).ok_or("could not determine a data directory (no $HOME)")?;
    fs::create_dir_all(&dest_root).map_err(|e| format!("creating {}: {e}", dest_root.display()))?;

    let release = fetch_release(&spec.repo, &spec.tag)?;
    let url = find_asset_url(&release, &spec.asset)?.to_string();

    let archive_path = std::env::temp_dir().join(format!("ctrlvim-fetch-{}", spec.asset));
    download_to(&url, &archive_path)?;

    let result = match detect_format(&spec.asset) {
        ArchiveFormat::TarGz => extract_tar_gz(&archive_path, &dest_root, spec.strip_components),
        ArchiveFormat::Zip => extract_zip(&archive_path, &dest_root, spec.strip_components),
        ArchiveFormat::Raw => {
            let out_name = spec.bin.as_deref().unwrap_or(&spec.asset);
            extract_raw(&archive_path, &dest_root, out_name)
        }
    };
    let _ = fs::remove_file(&archive_path);
    result?;

    match &spec.bin {
        Some(bin) => {
            let bin_path = dest_root.join(bin);
            mark_executable(&bin_path);
            Ok(bin_path)
        }
        None => Ok(dest_root),
    }
}

/// Entry point for `cvi tool <subcommand>`. Returns the process exit code —
/// `main.rs` calls this and exits directly, never starting the TUI.
pub fn run_cli(mut args: impl Iterator<Item = String>) -> i32 {
    match args.next().as_deref() {
        Some("fetch-release") => {}
        Some(other) => {
            eprintln!("cvi tool: unknown subcommand '{other}' (expected 'fetch-release')");
            return 2;
        }
        None => {
            eprintln!("cvi tool: expected a subcommand ('fetch-release')");
            return 2;
        }
    }
    let spec = match parse_args(args) {
        Ok(spec) => spec,
        Err(e) => {
            eprintln!("cvi tool fetch-release: {e}");
            return 2;
        }
    };
    match run(&spec) {
        Ok(path) => {
            println!("{}", path.display());
            0
        }
        Err(e) => {
            eprintln!("cvi tool fetch-release: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_the_required_flags_with_defaults() {
        let spec = parse_args(args(&["--repo", "LuaLS/lua-language-server", "--asset", "x.tar.gz", "--dest", "lua_ls"]))
            .unwrap();
        assert_eq!(spec.repo, "LuaLS/lua-language-server");
        assert_eq!(spec.tag, "latest", "tag defaults to latest");
        assert_eq!(spec.asset, "x.tar.gz");
        assert_eq!(spec.dest, "lua_ls");
        assert_eq!(spec.strip_components, 0);
        assert_eq!(spec.bin, None);
    }

    #[test]
    fn parses_every_optional_flag() {
        let spec = parse_args(args(&[
            "--repo",
            "o/r",
            "--tag",
            "v1.2.3",
            "--asset",
            "a.zip",
            "--dest",
            "name",
            "--strip-components",
            "1",
            "--bin",
            "bin/tool",
        ]))
        .unwrap();
        assert_eq!(spec.tag, "v1.2.3");
        assert_eq!(spec.strip_components, 1);
        assert_eq!(spec.bin.as_deref(), Some("bin/tool"));
    }

    #[test]
    fn a_missing_required_flag_is_an_error_not_a_panic() {
        assert!(parse_args(args(&["--asset", "x", "--dest", "y"])).is_err(), "missing --repo");
        assert!(parse_args(args(&["--repo", "o/r", "--dest", "y"])).is_err(), "missing --asset");
        assert!(parse_args(args(&["--repo", "o/r", "--asset", "x"])).is_err(), "missing --dest");
    }

    #[test]
    fn a_flag_missing_its_value_is_an_error() {
        assert!(parse_args(args(&["--repo"])).is_err());
    }

    #[test]
    fn an_unknown_flag_is_rejected() {
        let err = parse_args(args(&["--nope", "x"])).unwrap_err();
        assert!(err.contains("--nope"), "{err}");
    }

    #[test]
    fn strip_components_not_a_number_is_an_error() {
        let err =
            parse_args(args(&["--repo", "o/r", "--asset", "a", "--dest", "d", "--strip-components", "nope"]))
                .unwrap_err();
        assert!(err.contains("--strip-components"), "{err}");
    }

    #[test]
    fn release_url_uses_the_latest_endpoint_for_latest() {
        assert_eq!(release_url("LuaLS/lua-language-server", "latest"), "https://api.github.com/repos/LuaLS/lua-language-server/releases/latest");
    }

    #[test]
    fn release_url_uses_the_tags_endpoint_for_a_pinned_tag() {
        assert_eq!(release_url("o/r", "v1.2.3"), "https://api.github.com/repos/o/r/releases/tags/v1.2.3");
    }

    fn fixture_release() -> Release {
        Release {
            assets: vec![
                ReleaseAsset {
                    name: "tool-darwin-arm64.tar.gz".into(),
                    browser_download_url: "https://example.com/darwin-arm64.tar.gz".into(),
                },
                ReleaseAsset {
                    name: "tool-linux-x64.tar.gz".into(),
                    browser_download_url: "https://example.com/linux-x64.tar.gz".into(),
                },
            ],
        }
    }

    #[test]
    fn find_asset_url_matches_by_exact_name() {
        let release = fixture_release();
        assert_eq!(find_asset_url(&release, "tool-darwin-arm64.tar.gz").unwrap(), "https://example.com/darwin-arm64.tar.gz");
    }

    #[test]
    fn find_asset_url_lists_what_was_actually_available_on_a_miss() {
        let release = fixture_release();
        let err = find_asset_url(&release, "tool-windows.zip").unwrap_err();
        assert!(err.contains("tool-darwin-arm64.tar.gz"), "{err}");
        assert!(err.contains("tool-linux-x64.tar.gz"), "{err}");
    }

    #[test]
    fn detect_format_recognizes_tar_gz_tgz_zip_and_falls_back_to_raw() {
        assert_eq!(detect_format("x.tar.gz"), ArchiveFormat::TarGz);
        assert_eq!(detect_format("x.TAR.GZ"), ArchiveFormat::TarGz);
        assert_eq!(detect_format("x.tgz"), ArchiveFormat::TarGz);
        assert_eq!(detect_format("x.zip"), ArchiveFormat::Zip);
        assert_eq!(detect_format("gopls"), ArchiveFormat::Raw);
        assert_eq!(detect_format("tool.exe"), ArchiveFormat::Raw);
    }

    #[test]
    fn strip_prefix_components_drops_the_leading_wrapper_directory() {
        assert_eq!(
            strip_prefix_components(Path::new("lua-language-server-3.13.6/bin/lua-language-server"), 1),
            Some(PathBuf::from("bin/lua-language-server"))
        );
    }

    #[test]
    fn strip_prefix_components_none_when_nothing_is_stripped() {
        assert_eq!(strip_prefix_components(Path::new("bin/tool"), 0), Some(PathBuf::from("bin/tool")));
    }

    #[test]
    fn strip_prefix_components_is_none_when_it_consumes_the_whole_path() {
        // A bare top-level directory entry (e.g. `lua-language-server-3.13.6/`
        // itself) has nothing left once its own name is stripped.
        assert_eq!(strip_prefix_components(Path::new("lua-language-server-3.13.6"), 1), None);
    }

    #[test]
    fn strip_prefix_components_is_none_when_stripping_more_than_the_path_has() {
        assert_eq!(strip_prefix_components(Path::new("a/b"), 5), None);
    }
}
