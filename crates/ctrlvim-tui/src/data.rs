//! Real project data for the dashboard.
//!
//! Everything here reflects the actual project the editor is launched in (the
//! current working directory): recent files come from the filesystem, git
//! status from the `git` CLI, LOC from counting source lines, LSP servers from
//! probing `PATH`, and plugins from the conventional pack directory. Sources
//! that don't exist yield truthful empty state rather than mock data.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime};

use ratatui::style::Color;

use crate::model::{
    icon_for, FileEntry, GitStatus, LspServer, Plugin, PluginStatus, SessionEntry, Stats,
};

/// A gathered snapshot of the project.
pub struct Project {
    pub root: PathBuf,
    pub recent_files: Vec<FileEntry>,
    pub git: Option<GitStatus>,
    pub sessions: Vec<SessionEntry>,
    pub plugins: Vec<Plugin>,
    pub lsp: Vec<LspServer>,
    pub stats: Stats,
}

impl Project {
    /// Gather everything for `root`. `start` is the process start instant, used
    /// to report a real startup time.
    pub fn load(root: PathBuf, start: Instant) -> Self {
        let scanned = scan_files(&root);
        let recent_files = recent_files(&scanned);
        let loc = count_loc(&root, &scanned);
        let git = load_git(&root);
        let plugins = load_plugins();
        let lsp = detect_lsp();
        let sessions = load_sessions(&root, git.as_ref(), scanned.len());
        let plugins_loaded = plugins.iter().filter(|p| p.status == PluginStatus::Loaded).count();
        let stats = Stats {
            startup_ms: start.elapsed().as_millis(),
            plugins_loaded,
            plugins_total: plugins.len(),
            loc: group_thousands(loc),
        };
        Project { root, recent_files, git, sessions, plugins, lsp, stats }
    }
}

// --- filesystem scan -------------------------------------------------------

struct Scanned {
    /// Path relative to root, using `/` separators.
    rel: String,
    name: String,
    mtime: SystemTime,
    ext: String,
}

const SKIP_DIRS: &[&str] = &[
    ".git", "target", "node_modules", ".cargo", "dist", "build", ".venv",
    "__pycache__", ".next", ".idea", ".vscode", "vendor", ".mypy_cache",
];
const MAX_FILES: usize = 8000;
const MAX_DEPTH: usize = 6;

fn scan_files(root: &Path) -> Vec<Scanned> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if out.len() >= MAX_FILES {
            break;
        }
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            let fname = entry.file_name().to_string_lossy().into_owned();
            if ft.is_dir() {
                if depth + 1 > MAX_DEPTH || fname.starts_with('.') || SKIP_DIRS.contains(&fname.as_str()) {
                    continue;
                }
                stack.push((path, depth + 1));
            } else if ft.is_file() {
                let Ok(meta) = entry.metadata() else { continue };
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                let ext = fname.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
                out.push(Scanned { rel, name: fname, mtime, ext });
                if out.len() >= MAX_FILES {
                    break;
                }
            }
        }
    }
    out
}

fn recent_files(scanned: &[Scanned]) -> Vec<FileEntry> {
    let mut idx: Vec<&Scanned> = scanned.iter().collect();
    idx.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    idx.into_iter()
        .take(8)
        .map(|s| {
            let (letter, color) = icon_for(&s.name);
            FileEntry {
                name: s.name.clone(),
                path: s.rel.clone(),
                icon_color: color,
                icon_letter: letter,
                modified: humanize_ago(&s.mtime),
            }
        })
        .collect()
}

const CODE_EXTS: &[&str] = &[
    "rs", "toml", "lua", "md", "js", "jsx", "ts", "tsx", "json", "py", "c", "h",
    "cpp", "hpp", "cc", "go", "rb", "sh", "yaml", "yml", "html", "css", "txt",
];

fn count_loc(root: &Path, scanned: &[Scanned]) -> usize {
    scanned
        .iter()
        .filter(|s| CODE_EXTS.contains(&s.ext.as_str()))
        .take(4000)
        .filter_map(|s| fs::read_to_string(root.join(&s.rel)).ok())
        .map(|c| c.lines().count())
        .sum()
}

// --- file browser listing --------------------------------------------------

/// One row in the fuzzy file browser: name, disk metadata, and an icon.
#[derive(Clone)]
pub struct FinderEntry {
    /// Display name; directories keep a trailing `/`.
    pub name: String,
    /// Absolute path (for directories, the directory to descend into).
    pub path: PathBuf,
    pub is_dir: bool,
    /// `ls -l`-style permission bits, e.g. `drwxr-xr-x`.
    pub perms: String,
    /// Human size, e.g. `6.6K` (empty for the `../` row).
    pub size: String,
    /// Modified time, e.g. `Jul 06 13:24` (empty for the `../` row).
    pub mtime: String,
    pub icon_letter: char,
    pub icon_color: Color,
}

/// List `dir` for the browser: directories first, then files (each group
/// case-insensitively sorted), with a trailing `../` to ascend.
pub fn list_dir(dir: &Path) -> Vec<FinderEntry> {
    let (mut dirs, mut files): (Vec<FinderEntry>, Vec<FinderEntry>) = (Vec::new(), Vec::new());
    if let Ok(read) = fs::read_dir(dir) {
        for entry in read.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            let is_dir = meta.is_dir();
            let raw = entry.file_name().to_string_lossy().into_owned();
            let (icon_letter, icon_color) =
                if is_dir { ('/', crate::theme::blue()) } else { icon_for(&raw) };
            let e = FinderEntry {
                name: if is_dir { format!("{raw}/") } else { raw },
                path: entry.path(),
                is_dir,
                perms: perms_string(meta.permissions().mode(), is_dir),
                size: human_size(meta.len()),
                mtime: fmt_mtime(meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)),
                icon_letter,
                icon_color,
            };
            if is_dir { dirs.push(e) } else { files.push(e) }
        }
    }
    let by_name = |a: &FinderEntry, b: &FinderEntry| {
        a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase())
    };
    dirs.sort_by(by_name);
    files.sort_by(by_name);
    dirs.append(&mut files);
    // `../` pinned at the bottom, unless we're already at the filesystem root.
    if let Some(parent) = dir.parent() {
        dirs.push(FinderEntry {
            name: "../".into(),
            perms: fs::metadata(parent)
                .map(|m| perms_string(m.permissions().mode(), true))
                .unwrap_or_else(|_| "drwxr-xr-x".into()),
            path: parent.to_path_buf(),
            is_dir: true,
            size: String::new(),
            mtime: String::new(),
            icon_letter: '/',
            icon_color: crate::theme::blue(),
        });
    }
    dirs
}

/// Render Unix mode bits as a `drwxr-xr-x` string.
fn perms_string(mode: u32, is_dir: bool) -> String {
    let mut s = String::with_capacity(10);
    s.push(if is_dir { 'd' } else { '-' });
    const BITS: [(u32, char); 9] = [
        (0o400, 'r'), (0o200, 'w'), (0o100, 'x'),
        (0o040, 'r'), (0o020, 'w'), (0o010, 'x'),
        (0o004, 'r'), (0o002, 'w'), (0o001, 'x'),
    ];
    for (bit, ch) in BITS {
        s.push(if mode & bit != 0 { ch } else { '-' });
    }
    s
}

/// Human-readable byte size: `< 1024` shown raw, otherwise `6.6K` / `10K` / `1.2M`.
fn human_size(bytes: u64) -> String {
    if bytes < 1024 {
        return bytes.to_string();
    }
    const UNITS: [&str; 4] = ["K", "M", "G", "T"];
    let mut v = bytes as f64 / 1024.0;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if v < 10.0 {
        format!("{v:.1}{}", UNITS[u])
    } else {
        format!("{v:.0}{}", UNITS[u])
    }
}

/// Format a modification time as `Mon DD HH:MM` (UTC — good enough for a list).
fn fmt_mtime(t: SystemTime) -> String {
    let secs = t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (days, tod) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    let (hour, min) = (tod / 3600, (tod % 3600) / 60);
    let (_y, m, d) = civil_from_days(days);
    const MON: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!("{} {:02} {:02}:{:02}", MON[(m - 1) as usize], d, hour, min)
}

/// Howard Hinnant's civil-from-days: `z` days since 1970-01-01 → (year, month, day).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// --- git -------------------------------------------------------------------

fn load_git(root: &Path) -> Option<GitStatus> {
    let out = Command::new("git")
        .args(["-C"])
        .arg(root)
        .args(["status", "--porcelain=v2", "--branch"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);

    let mut branch = "(detached)".to_string();
    let mut remote = String::new();
    let (mut ahead, mut behind) = (0u32, 0u32);
    let (mut staged, mut modified, mut untracked) = (0u32, 0u32, 0u32);

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            branch = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("# branch.upstream ") {
            remote = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            // e.g. "+3 -0"
            for tok in rest.split_whitespace() {
                if let Some(n) = tok.strip_prefix('+') {
                    ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = tok.strip_prefix('-') {
                    behind = n.parse().unwrap_or(0);
                }
            }
        } else if let Some(rest) = line.strip_prefix("1 ").or_else(|| line.strip_prefix("2 ")) {
            // "<XY> ..." — X = staged status, Y = worktree status.
            let xy: Vec<char> = rest.chars().take(2).collect();
            if xy.first().is_some_and(|&c| c != '.') {
                staged += 1;
            }
            if xy.get(1).is_some_and(|&c| c != '.') {
                modified += 1;
            }
        } else if line.starts_with("? ") {
            untracked += 1;
        }
    }

    let last_commit = Command::new("git")
        .args(["-C"])
        .arg(root)
        .args(["log", "-1", "--format=%cr"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "—".to_string());

    Some(GitStatus { branch, ahead, behind, modified, staged, remote, last_commit, untracked })
}

// --- lsp / plugins / sessions ---------------------------------------------

fn detect_lsp() -> Vec<LspServer> {
    // Language servers are named after their nvim-lspconfig identifiers so the
    // same config vocabulary carries over; build linkers are surfaced as extra
    // rows (filetypes = "linker"). (name, filetypes, candidate binaries on PATH)
    let known: &[(&str, &str, &[&str])] = &[
        ("rust_analyzer", "rust", &["rust-analyzer"]),
        ("taplo", "toml", &["taplo"]),
        ("lua_ls", "lua", &["lua-language-server", "lua_ls"]),
        ("marksman", "markdown", &["marksman"]),
        ("ts_ls", "ts, tsx, js, jsx", &["typescript-language-server", "tsserver"]),
        ("jdtls", "java (maven/gradle)", &["jdtls"]),
        ("lemminx", "xml (pom.xml)", &["lemminx"]),
        ("mesonlsp", "meson", &["mesonlsp", "Swift-MesonLSP"]),
        // Build linkers.
        ("mold", "linker", &["mold", "ld.mold"]),
        ("lld", "linker", &["ld.lld", "lld"]),
        ("wild", "linker", &["wild"]),
        ("gold", "linker", &["ld.gold", "gold"]),
        ("ld.bfd", "linker", &["ld.bfd", "ld"]),
    ];
    known
        .iter()
        .map(|(name, ft, bins)| LspServer {
            name: name.to_string(),
            filetypes: ft.to_string(),
            installed: bins.iter().any(|b| on_path(b)),
        })
        .collect()
}

/// Scan the conventional pack directory for installed plugins.
/// `<config>/ctrlvim/pack/*/start/*` are loaded, `.../opt/*` are lazy.
fn load_plugins() -> Vec<Plugin> {
    let Some(pack) = config_dir().map(|c| c.join("ctrlvim").join("pack")) else {
        return Vec::new();
    };
    let mut plugins = Vec::new();
    let Ok(groups) = fs::read_dir(&pack) else { return plugins };
    for group in groups.flatten() {
        for (sub, status) in [("start", PluginStatus::Loaded), ("opt", PluginStatus::Lazy)] {
            let dir = group.path().join(sub);
            let Ok(entries) = fs::read_dir(&dir) else { continue };
            for e in entries.flatten() {
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    let name = e.file_name().to_string_lossy().into_owned();
                    let repo = git_remote(&e.path()).unwrap_or_default();
                    plugins.push(Plugin { name, repo, category: sub.to_string(), status });
                }
            }
        }
    }
    plugins.sort_by(|a, b| a.name.cmp(&b.name));
    plugins
}

fn load_sessions(root: &Path, git: Option<&GitStatus>, file_count: usize) -> Vec<SessionEntry> {
    let mut sessions = Vec::new();

    // Persisted store, if any: TSV of `path\tbranch\tfiles\tlast`.
    if let Some(store) = state_dir().map(|s| s.join("ctrlvim").join("sessions.tsv")) {
        if let Ok(text) = fs::read_to_string(&store) {
            for line in text.lines() {
                let f: Vec<&str> = line.split('\t').collect();
                if f.len() == 4 {
                    let name = Path::new(f[0])
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| f[0].to_string());
                    sessions.push(SessionEntry {
                        name,
                        branch: f[1].to_string(),
                        files: f[2].parse().unwrap_or(0),
                        last: f[3].to_string(),
                    });
                }
            }
        }
    }

    // Always include the current project as the most recent session.
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
    let branch = git.map(|g| g.branch.clone()).unwrap_or_else(|| "—".to_string());
    if !sessions.iter().any(|s| s.name == name) {
        sessions.insert(0, SessionEntry { name, branch, files: file_count as u32, last: "this session" .to_string() });
    }
    sessions.truncate(6);
    sessions
}

// --- small helpers ---------------------------------------------------------

fn on_path(bin: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file())
}

fn git_remote(dir: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["-C"])
        .arg(dir)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // Trim to `owner/repo` for display.
    let short = url
        .trim_end_matches(".git")
        .rsplit(['/', ':'])
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("/");
    (!short.is_empty()).then_some(short)
}

pub(crate) fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| home().map(|h| h.join(".config")))
}

/// Path of the file the chosen theme name is persisted to.
fn theme_store() -> Option<PathBuf> {
    state_dir().map(|s| s.join("ctrlvim").join("theme"))
}

/// The theme name saved from a previous session, if any.
pub fn saved_theme() -> Option<String> {
    let path = theme_store()?;
    let name = std::fs::read_to_string(path).ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Persist the chosen theme name so the next launch restores it. Best-effort:
/// failures (no state dir, unwritable) are silently ignored.
pub fn save_theme(name: &str) {
    if let Some(path) = theme_store() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, name);
    }
}

fn state_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| home().map(|h| h.join(".local").join("state")))
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn humanize_ago(t: &SystemTime) -> String {
    let secs = SystemTime::now().duration_since(*t).map(|d| d.as_secs()).unwrap_or(0);
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{} min ago", secs / 60)
    } else if secs < 86400 {
        format!("{} hr ago", secs / 3600)
    } else {
        let d = secs / 86400;
        if d == 1 { "1 day ago".to_string() } else { format!("{d} days ago") }
    }
}

fn group_thousands(n: usize) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::new();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}
