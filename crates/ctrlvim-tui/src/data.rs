//! Real project data for the dashboard.
//!
//! Everything here reflects the actual project the editor is launched in (the
//! current working directory): recent files come from the filesystem, git
//! status from the `git` CLI, LOC from counting source lines, LSP servers from
//! probing `PATH`, and plugins from the conventional pack directory. Sources
//! that don't exist yield truthful empty state rather than mock data.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime};

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
    // (display name, filetypes, candidate binaries on PATH)
    let known: &[(&str, &str, &[&str])] = &[
        ("rust-analyzer", "rust", &["rust-analyzer"]),
        ("taplo", "toml", &["taplo"]),
        ("lua_ls", "lua", &["lua-language-server", "lua_ls"]),
        ("marksman", "markdown", &["marksman"]),
        ("tsserver", "ts, tsx, js, jsx", &["typescript-language-server", "tsserver"]),
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

fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| home().map(|h| h.join(".config")))
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
