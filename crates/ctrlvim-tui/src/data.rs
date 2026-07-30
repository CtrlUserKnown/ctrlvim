//! Real project data for the dashboard.
//!
//! Everything here reflects the actual project the editor is launched in (the
//! current working directory): recent files come from the filesystem, git
//! status from the `git` CLI, LOC from counting source lines, language
//! servers/formatters/linters from `ctrlvim_tools::REGISTRY` (PATH probing
//! plus ctrlvim's own managed tools directory), and plugins from the
//! conventional pack directory. Sources that don't exist yield truthful empty
//! state rather than mock data.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime};

use crate::model::{
    icon_for, FileEntry, FileIcon, GitChange, GitStatus, LspServer, Plugin, PluginStatus,
    SessionEntry, Stats,
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

/// Walk the project for files worth listing or searching, skipping VCS/build
/// directories and dotfiles and bounded by depth and count.
///
/// This is the one definition of "which files count" — the dashboard's recent
/// files and `:vimgrep` both go through it, so they can never disagree about
/// whether `target/` is part of the project.
pub fn walk_project(root: &Path) -> Vec<PathBuf> {
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
                out.push(path);
                if out.len() >= MAX_FILES {
                    break;
                }
            }
        }
    }
    // `read_dir` order is whatever the filesystem hands back; sorting makes
    // quickfix entries and the file list stable between runs.
    out.sort();
    out
}

/// A project path relative to `root`, with `/` separators on every platform.
pub fn relative_to(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

fn scan_files(root: &Path) -> Vec<Scanned> {
    walk_project(root)
        .into_iter()
        .filter_map(|path| {
            let meta = fs::metadata(&path).ok()?;
            let name = path.file_name()?.to_string_lossy().into_owned();
            let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
            Some(Scanned {
                rel: relative_to(root, &path),
                name,
                mtime: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                ext,
            })
        })
        .collect()
}

fn recent_files(scanned: &[Scanned]) -> Vec<FileEntry> {
    let mut idx: Vec<&Scanned> = scanned.iter().collect();
    idx.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    idx.into_iter()
        .take(8)
        .map(|s| {
            FileEntry {
                name: s.name.clone(),
                path: s.rel.clone(),
                icon: icon_for(&s.name),
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
    pub icon: FileIcon,
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
            let icon = if is_dir { FileIcon::dir() } else { icon_for(&raw) };
            let e = FinderEntry {
                name: if is_dir { format!("{raw}/") } else { raw },
                path: entry.path(),
                is_dir,
                perms: perms_string(meta.permissions().mode(), is_dir),
                size: human_size(meta.len()),
                mtime: fmt_mtime(meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)),
                icon,
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
            icon: FileIcon::dir(),
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

/// The path field of a porcelain-v2 record, given how many space-separated
/// fields precede it.
///
/// `splitn` rather than `split_whitespace().nth()`: porcelain v2 prints paths
/// literally, so a file with a space in its name would otherwise be truncated
/// at the space — and the path is always the *last* field, so taking the
/// remainder is exactly right.
fn porcelain_path(rest: &str, fields_before: usize) -> Option<String> {
    let mut parts = rest.splitn(fields_before + 1, ' ');
    let path = parts.nth(fields_before)?;
    (!path.is_empty()).then(|| path.to_string())
}

fn push_change(out: &mut Vec<GitChange>, path: Option<String>, label: &'static str) {
    if let Some(path) = path {
        out.push(GitChange { path, label });
    }
}

/// Count the lines of a command's output, or 0 if it can't run.
fn count_lines(root: &Path, args: &[&str]) -> u32 {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().count() as u32)
        .unwrap_or(0)
}

/// Parse `git diff --shortstat`'s " N files changed, I insertions(+), D deletions(-)".
/// Either half is absent when it is zero, so both are looked up by their word.
fn parse_shortstat(text: &str) -> (u32, u32) {
    let num_before = |word: &str| -> u32 {
        let idx = match text.find(word) {
            Some(i) => i,
            None => return 0,
        };
        text[..idx]
            .split_whitespace()
            .next_back()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0)
    };
    (num_before("insertion"), num_before("deletion"))
}

/// Re-read the git panel's data, for after a command that may have changed it.
pub fn reload_git(root: &Path) -> Option<GitStatus> {
    load_git(root)
}

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
    let mut oid = "—".to_string();
    let (mut ahead, mut behind) = (0u32, 0u32);
    let (mut staged, mut modified, mut untracked, mut conflicted) = (0u32, 0u32, 0u32, 0u32);
    let mut changed: Vec<GitChange> = Vec::new();

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            branch = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("# branch.oid ") {
            // Literally "(initial)" before the first commit, which is already
            // the right thing to show.
            let raw = rest.trim();
            oid = match raw.starts_with('(') {
                true => "—".to_string(),
                false => raw.chars().take(7).collect(),
            };
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
        } else if let Some(rest) = line.strip_prefix("1 ") {
            // "<XY> ..." — X = staged status, Y = worktree status.
            let xy: Vec<char> = rest.chars().take(2).collect();
            let is_staged = xy.first().is_some_and(|&c| c != '.');
            if is_staged {
                staged += 1;
            }
            if xy.get(1).is_some_and(|&c| c != '.') {
                modified += 1;
            }
            push_change(&mut changed, porcelain_path(rest, 7), if is_staged { "staged" } else { "modified" });
        } else if let Some(rest) = line.strip_prefix("2 ") {
            // Renamed / copied: one extra field (the similarity score) before
            // the path, and the path itself is "new\told".
            let xy: Vec<char> = rest.chars().take(2).collect();
            let is_staged = xy.first().is_some_and(|&c| c != '.');
            if is_staged {
                staged += 1;
            }
            if xy.get(1).is_some_and(|&c| c != '.') {
                modified += 1;
            }
            let path = porcelain_path(rest, 8).map(|p| p.split('\t').next().unwrap_or(&p).to_string());
            push_change(&mut changed, path, "renamed");
        } else if let Some(rest) = line.strip_prefix("u ") {
            // Unmerged. Git gives these their own record type rather than an
            // `XY` pair, so the branch above never sees them — counting them
            // there would also be wrong, since a conflict is not an edit you
            // can stage away.
            conflicted += 1;
            push_change(&mut changed, porcelain_path(rest, 9), "conflict");
        } else if let Some(rest) = line.strip_prefix("? ") {
            untracked += 1;
            push_change(&mut changed, Some(rest.to_string()), "untracked");
        }
    }
    // Conflicts first, then staged, then the rest: the quickfix list `[c]`
    // builds from this is walked top-down, so it should open with what most
    // needs attention.
    changed.sort_by_key(|c| match c.label {
        "conflict" => 0,
        "staged" => 1,
        "modified" => 2,
        "renamed" => 3,
        _ => 4,
    });

    // Relative time, author and subject in one call — three `git log`
    // invocations to fill three adjacent rows would be silly.
    let log = Command::new("git")
        .args(["-C"])
        .arg(root)
        .args(["log", "-1", "--format=%cr%n%an%n%s"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let mut log_lines = log.lines();
    let field = |v: Option<&str>| match v.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => "—".to_string(),
    };
    let last_commit = field(log_lines.next());
    let last_author = field(log_lines.next());
    let last_subject = field(log_lines.next());

    let stashes = count_lines(root, &["stash", "list"]);
    // Against HEAD, so staged and unstaged work both count — the panel's
    // `+N −M` is "how far this tree is from the last commit", which is the
    // question the number next to `modified` is actually answering.
    let shortstat = Command::new("git")
        .args(["-C"])
        .arg(root)
        .args(["diff", "--shortstat", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let (insertions, deletions) = parse_shortstat(&shortstat);

    Some(GitStatus {
        branch, oid, ahead, behind, modified, staged, remote,
        last_commit, last_author, last_subject,
        untracked, conflicted, stashes, insertions, deletions, changed,
    })
}

// --- lsp / plugins / sessions ---------------------------------------------

fn detect_lsp() -> Vec<LspServer> {
    // Language servers, formatters, and linters come from ctrlvim's own tool
    // registry (see `ctrlvim_tools`), which also knows how to install the
    // ones it can. Build linkers aren't part of that registry — they're
    // system packages, not per-project dev tools — so they stay a handful of
    // pure PATH-detection rows, same as before.
    let mut tools: Vec<LspServer> = ctrlvim_tools::REGISTRY.iter().map(lsp_server_from_tool).collect();

    let linkers: &[(&str, &[&str])] = &[
        ("mold", &["mold", "ld.mold"]),
        ("lld", &["ld.lld", "lld"]),
        ("wild", &["wild"]),
        ("gold", &["ld.gold", "gold"]),
        ("ld.bfd", &["ld.bfd", "ld"]),
    ];
    tools.extend(linkers.iter().map(|(name, bins)| LspServer {
        name: name.to_string(),
        filetypes: "linker".to_string(),
        installed: bins.iter().any(|b| on_path(b)),
        managed: false,
        installable: false,
    }));
    tools
}

pub(crate) fn lsp_server_from_tool(tool: &ctrlvim_tools::Tool) -> LspServer {
    let (installed, managed) = ctrlvim_tools::installed_and_managed(tool);
    LspServer {
        name: tool.name.to_string(),
        filetypes: tool.filetypes.to_string(),
        installed,
        managed,
        installable: tool.method.is_supported(),
    }
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

/// Where a project's pinned files are remembered, keyed by project root.
///
/// Pins are per-project by nature — "the four files I'm working in" means
/// nothing across repositories — so the file name is derived from the root
/// rather than there being one global list. Kept in the state dir next to
/// `sessions.tsv`, since it's recoverable cache, not configuration.
pub fn pins_path(root: &Path) -> Option<PathBuf> {
    state_dir().map(|s| pins_path_in(&s, root))
}

/// The pins file for `root` under an explicit state directory. Split out from
/// [`pins_path`] so the read/write pair can be tested against a temp dir
/// instead of the developer's real `~/.local/state`.
fn pins_path_in(state: &Path, root: &Path) -> PathBuf {
    state.join("ctrlvim").join("pins").join(format!("{}.txt", sanitize_path_key(root)))
}

/// Turn an absolute path into a filesystem-safe key: every non-alphanumeric
/// byte becomes `_`. Used to derive a per-project (or per-file) state file
/// name from a path that may contain `/` and other separators.
pub(crate) fn sanitize_path_key(path: &Path) -> String {
    path.to_string_lossy().chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect()
}

/// Where a project's restorable editor session lives: the open buffer list
/// (`index.tsv`) and, for any buffer with unsaved changes, a `recovery/`
/// snapshot of its live text — so a crash or an accidental window close loses
/// neither which files were open nor what was typed into them. Keyed by
/// project root the same way [`pins_path`] is, since "what was open" is as
/// project-specific as "what's pinned".
pub fn session_dir(root: &Path) -> Option<PathBuf> {
    state_dir().map(|s| s.join("ctrlvim").join("sessions").join(sanitize_path_key(root)))
}

/// Read a project's pinned files. Missing or unreadable is an empty list —
/// losing pins is not worth refusing to start over.
pub fn load_pins(root: &Path) -> Vec<PathBuf> {
    let Some(state) = state_dir() else { return Vec::new() };
    load_pins_in(&state, root)
}

fn load_pins_in(state: &Path, root: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(pins_path_in(state, root)) else { return Vec::new() };
    text.lines().filter(|l| !l.trim().is_empty()).map(PathBuf::from).collect()
}

/// Write a project's pinned files (best-effort).
pub fn save_pins(root: &Path, files: &[PathBuf]) {
    if let Some(state) = state_dir() {
        save_pins_in(&state, root, files);
    }
}

fn save_pins_in(state: &Path, root: &Path, files: &[PathBuf]) {
    let path = pins_path_in(state, root);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let body: String = files.iter().map(|f| format!("{}\n", f.to_string_lossy())).collect();
    let _ = std::fs::write(path, body);
}

fn state_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| home().map(|h| h.join(".local").join("state")))
}

pub(crate) fn home() -> Option<PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temp directory, so the tests never touch the real state dir.
    fn temp_state() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ctrlvim-pins-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Run `git` in `dir`, failing loudly — a silently-skipped setup step
    /// would make the assertions below pass for the wrong reason.
    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git").arg("-C").arg(dir).args(args).output().expect("git runs");
        assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn shortstat_survives_a_missing_half() {
        assert_eq!(parse_shortstat(" 11 files changed, 248 insertions(+), 34 deletions(-)"), (248, 34));
        // git omits whichever side is zero.
        assert_eq!(parse_shortstat(" 1 file changed, 3 insertions(+)"), (3, 0));
        assert_eq!(parse_shortstat(" 1 file changed, 9 deletions(-)"), (0, 9));
        // Singular forms, and nothing at all.
        assert_eq!(parse_shortstat(" 1 file changed, 1 insertion(+), 1 deletion(-)"), (1, 1));
        assert_eq!(parse_shortstat(""), (0, 0));
    }

    #[test]
    fn a_porcelain_path_may_contain_spaces() {
        // The path is the last field, so it must be taken as the remainder —
        // splitting on whitespace would cut "my notes.md" in half.
        let rest = "1 .M N... 100644 100644 100644 aaa bbb my notes.md";
        let rest = rest.strip_prefix("1 ").unwrap();
        assert_eq!(porcelain_path(rest, 7).as_deref(), Some("my notes.md"));
    }

    #[test]
    fn changed_files_are_listed_conflicts_first() {
        let dir = temp_state();
        git(&dir, &["init", "-q", "-b", "main", "."]);
        git(&dir, &["config", "user.email", "t@example.com"]);
        git(&dir, &["config", "user.name", "Test"]);
        std::fs::write(dir.join("tracked.txt"), "one\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "base"]);

        std::fs::write(dir.join("tracked.txt"), "edited\n").unwrap();
        std::fs::write(dir.join("new.txt"), "fresh\n").unwrap();

        let g = load_git(&dir).expect("a git repo");
        let paths: Vec<&str> = g.changed.iter().map(|c| c.path.as_str()).collect();
        assert!(paths.contains(&"tracked.txt"), "got {paths:?}");
        assert!(paths.contains(&"new.txt"), "untracked files are listed too: {paths:?}");
        // Untracked sorts last, so the edited file comes first.
        assert_eq!(g.changed.first().map(|c| c.label), Some("modified"));
        assert_eq!(g.changed.last().map(|c| c.label), Some("untracked"));
        assert!(g.insertions > 0 || g.deletions > 0, "the diff stat is populated");
        assert_ne!(g.oid, "—", "a repo with a commit has a HEAD oid");
    }

    #[test]
    fn a_conflicted_tree_is_not_reported_as_clean() {
        let dir = temp_state();
        git(&dir, &["init", "-q", "-b", "main", "."]);
        git(&dir, &["config", "user.email", "t@example.com"]);
        git(&dir, &["config", "user.name", "Test"]);
        std::fs::write(dir.join("f.txt"), "one\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "base"]);

        git(&dir, &["checkout", "-q", "-b", "other"]);
        std::fs::write(dir.join("f.txt"), "theirs\n").unwrap();
        git(&dir, &["commit", "-qam", "theirs"]);
        git(&dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("f.txt"), "ours\n").unwrap();
        git(&dir, &["commit", "-qam", "ours"]);
        // This one is *expected* to fail — it is the conflict under test.
        let _ = Command::new("git").arg("-C").arg(&dir).args(["merge", "other"]).output();

        let g = load_git(&dir).expect("a git repo");
        assert_eq!(g.conflicted, 1, "the unmerged file must be counted");
        // The regression: `u` records are neither staged nor modified, so
        // before this was counted the panel showed 0/0 mid-conflict and read
        // as a clean tree.
        assert_eq!((g.staged, g.modified), (0, 0), "git reports `u` as neither");
    }

    #[test]
    fn a_clean_tree_reports_no_conflicts() {
        let dir = temp_state();
        git(&dir, &["init", "-q", "-b", "main", "."]);
        git(&dir, &["config", "user.email", "t@example.com"]);
        git(&dir, &["config", "user.name", "Test"]);
        std::fs::write(dir.join("f.txt"), "one\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "base"]);

        let g = load_git(&dir).expect("a git repo");
        assert_eq!((g.conflicted, g.staged, g.modified, g.untracked), (0, 0, 0, 0));
        assert_eq!(g.branch, "main");
    }

    #[test]
    fn pins_round_trip_through_the_state_dir() {
        let state = temp_state();
        let root = Path::new("/home/dev/project");
        assert!(load_pins_in(&state, root).is_empty(), "nothing saved yet");

        let files = vec![PathBuf::from("src/main.rs"), PathBuf::from("src/lib.rs")];
        save_pins_in(&state, root, &files);
        assert_eq!(load_pins_in(&state, root), files, "order is preserved");

        std::fs::remove_dir_all(&state).ok();
    }

    #[test]
    fn pins_are_kept_per_project() {
        // "the files I'm working in" means nothing across repositories, so two
        // roots must not share a list.
        let state = temp_state();
        save_pins_in(&state, Path::new("/a"), &[PathBuf::from("a.rs")]);
        save_pins_in(&state, Path::new("/b"), &[PathBuf::from("b.rs")]);
        assert_eq!(load_pins_in(&state, Path::new("/a")), [PathBuf::from("a.rs")]);
        assert_eq!(load_pins_in(&state, Path::new("/b")), [PathBuf::from("b.rs")]);

        std::fs::remove_dir_all(&state).ok();
    }

    #[test]
    fn saving_an_empty_list_clears_the_file() {
        let state = temp_state();
        let root = Path::new("/p");
        save_pins_in(&state, root, &[PathBuf::from("a.rs")]);
        save_pins_in(&state, root, &[]);
        assert!(load_pins_in(&state, root).is_empty(), ":PinClear must actually persist");

        std::fs::remove_dir_all(&state).ok();
    }
}
