//! Galleries (docs/specs/agent/06.8-galleries-the-game-photographs-itself.md,
//! docs/specs/tray/05.3-private-galleries-and-pictures-from.md): a gallery's `command` puts the
//! game into every subject in turn, screenshots it, writes PNGs to `outDir` and exits; `shoot`
//! runs that command on the project's base checkout and converts what it wrote into small JPEGs
//! kept at `<dataDir>/projects/<project>/galleries/<slug>/`, replacing the old set only once the
//! new one is all written.
//!
//! This is the shoot-and-shrink pipeline on its own: it runs the command, converts its pictures,
//! and tracks a gallery's status in memory. Still to come, once there's something to wire it to:
//! answering `gallery_index`/`gallery_image` from the hub (including the further shrink a picture
//! over 768 KiB needs before it's sent), keeping a shot gallery's set in the backlog store,
//! refusing a shoot while the baseline, Play main or a sync holds the base checkout (none of
//! which exist yet either), and the settings page that lets someone set a gallery's command.
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use image::codecs::jpeg::JpegEncoder;
use serde::{Deserialize, Serialize};

use crate::config::GalleryConfig;
use crate::lanes;

const SHOOT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const JPEG_QUALITY: u8 = 82;
const MAX_WIDTH: u32 = 1920;
/// How much of a failed or timed-out command's combined output is kept, and shown on failure.
const OUTPUT_CAP: usize = 20_000;
const TAIL_LINES: usize = 40;

/// One picture in a gallery, as the game's own manifest names it or, failing that, its file name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GalleryImage {
    pub file: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

/// A gallery's `index.json`: the set of pictures from its last shoot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GalleryIndex {
    pub name: String,
    pub shot_at: String,
    pub sha: String,
    pub images: Vec<GalleryImage>,
}

/// A gallery's `status` (tray.md section 5.3.3): `shooting`, then `converting`, then `idle` with
/// a new set, or `failed` with the command's or the conversion's error.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum GalleryStatus {
    #[default]
    Idle,
    Shooting,
    Converting,
    Failed(String),
}

/// One entry of the game's own `index.json` in `outDir`, naming a picture and, optionally, its
/// title and group.
#[derive(Debug, Deserialize)]
struct ManifestEntry {
    file: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    group: Option<String>,
}

/// Keyed by project id and gallery slug.
type GalleryKey = (String, String);

/// Every gallery's status, kept across shoots until the process restarts.
static STATUS: LazyLock<Mutex<HashMap<GalleryKey, GalleryStatus>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
/// The gallery each project is shooting, if any: only one shoot holds a project's base checkout
/// at a time (06.8: "Only one gallery shoots at a time").
static SHOOTING: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn set_status(project: &str, slug: &str, status: GalleryStatus) {
    STATUS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert((project.to_string(), slug.to_string()), status);
}

/// A gallery's current status, `idle` before its first shoot.
pub fn status(project: &str, slug: &str) -> GalleryStatus {
    STATUS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(project.to_string(), slug.to_string()))
        .cloned()
        .unwrap_or_default()
}

/// True while a shoot holds this project's base checkout.
pub fn busy(project: &str) -> bool {
    SHOOTING.lock().unwrap_or_else(|e| e.into_inner()).contains_key(project)
}

/// Why Shoot can't start now, or `None` (tray.md section 5.3.3). Checked again right before the
/// command runs: a gallery's command or out dir may have changed, or another shoot may have
/// started, since Shoot was first asked for.
pub fn shoot_refusal(project: &str, gallery: &GalleryConfig) -> Option<String> {
    if gallery.command.trim().is_empty() {
        return Some("No command set for this gallery.".to_string());
    }
    if safe_out_dir(&gallery.out_dir).is_none() {
        return Some(format!(
            "The output folder must be inside the repo: {}",
            gallery.out_dir
        ));
    }
    if busy(project) {
        return Some("Already shooting.".to_string());
    }
    None
}

/// Runs `gallery`'s command on `project`'s base checkout and replaces its pictures at
/// `gallery_dest` (`paths::gallery_dir`). Blocks until the shoot and conversion are done; a
/// caller that wants to poll [`status`] meanwhile runs this on its own thread.
pub fn shoot(
    project: &str,
    slug: &str,
    gallery: &GalleryConfig,
    repo_path: &Path,
    worktrees_dir: &Path,
    base_branch: &str,
    gallery_dest: &Path,
) -> Result<GalleryIndex> {
    if let Some(reason) = shoot_refusal(project, gallery) {
        bail!(reason);
    }
    let out_dir = safe_out_dir(&gallery.out_dir).expect("checked by shoot_refusal");
    SHOOTING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(project.to_string(), gallery.name.clone());
    set_status(project, slug, GalleryStatus::Shooting);
    let result = run_shoot(
        gallery,
        &out_dir,
        repo_path,
        worktrees_dir,
        base_branch,
        slug,
        project,
        gallery_dest,
    );
    SHOOTING.lock().unwrap_or_else(|e| e.into_inner()).remove(project);
    match &result {
        Ok(_) => set_status(project, slug, GalleryStatus::Idle),
        Err(e) => set_status(project, slug, GalleryStatus::Failed(e.to_string())),
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn run_shoot(
    gallery: &GalleryConfig,
    out_dir: &str,
    repo_path: &Path,
    worktrees_dir: &Path,
    base_branch: &str,
    slug: &str,
    project: &str,
    gallery_dest: &Path,
) -> Result<GalleryIndex> {
    let dir =
        lanes::prepare_base_checkout(repo_path, worktrees_dir, base_branch).context("preparing the base checkout")?;
    let sha = lanes::head_sha(&dir).context("reading the base checkout's commit")?;
    let out = dir.join(out_dir);
    let _ = std::fs::remove_dir_all(&out);
    let (code, output, timed_out) = run_command(&gallery.command, &dir)?;
    if code != 0 || timed_out {
        let what = if timed_out {
            "timed out".to_string()
        } else {
            format!("failed ({code})")
        };
        bail!("`{}` {what}:\n{}", gallery.command, tail_lines(&output, TAIL_LINES));
    }
    set_status(project, slug, GalleryStatus::Converting);
    convert(&out, &gallery.name, &sha, gallery_dest)
}

/// Runs `command` in a shell under `cwd`, with a window (it may need to render) and no input,
/// for up to [`SHOOT_TIMEOUT`]. Returns its exit code (-1 if it couldn't be read), combined
/// stdout/stderr (capped at [`OUTPUT_CAP`] bytes), and whether it was killed for running too long.
fn run_command(command: &str, cwd: &Path) -> Result<(i32, String, bool)> {
    let mut child = shell_command(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting `{command}`"))?;
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let buf = std::sync::Arc::new(Mutex::new(Vec::<u8>::new()));
    let t_out = std::thread::spawn({
        let buf = buf.clone();
        move || capture_capped(stdout, buf)
    });
    let t_err = std::thread::spawn({
        let buf = buf.clone();
        move || capture_capped(stderr, buf)
    });
    let deadline = Instant::now() + SHOOT_TIMEOUT;
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            timed_out = true;
            let _ = lanes::kill_processes_in(cwd);
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let _ = t_out.join();
    let _ = t_err.join();
    let output_bytes = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let output = String::from_utf8_lossy(&output_bytes).into_owned();
    let code = status.and_then(|s| s.code()).unwrap_or(-1);
    Ok((code, output, timed_out))
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    let mut c = Command::new("cmd");
    c.args(["/C", command]);
    c
}

#[cfg(not(windows))]
fn shell_command(command: &str) -> Command {
    let mut c = Command::new("sh");
    c.args(["-c", command]);
    c
}

/// Reads `pipe` to EOF into `buf`, kept to the last [`OUTPUT_CAP`] bytes, like the TypeScript
/// agent's `output.slice(-20_000)` while a shoot's command is still running.
fn capture_capped(mut pipe: impl Read, buf: std::sync::Arc<Mutex<Vec<u8>>>) {
    let mut chunk = [0u8; 4096];
    loop {
        let n = match pipe.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        let mut buf = buf.lock().unwrap_or_else(|e| e.into_inner());
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > OUTPUT_CAP {
            let drop = buf.len() - OUTPUT_CAP;
            buf.drain(..drop);
        }
    }
}

/// The last `n` lines of `text` (06.8: a failed shoot's `failed` status carries "the output's
/// last 40 lines").
fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// `outDir` must be a relative path that stays inside the repo: no absolute path (POSIX or a
/// Windows drive letter, checked regardless of the host OS), and no `..` segment. `None` for
/// `outDir` empty or equal to `.` (Shoot empties it first, so the repo itself is refused too).
pub fn safe_out_dir(out_dir: &str) -> Option<String> {
    let raw = out_dir.replace('\\', "/");
    let raw = raw.trim();
    // `Path::is_absolute` only recognizes Windows-rooted paths on Windows and POSIX-rooted ones on
    // POSIX, but this must refuse both regardless of which OS the agent itself runs on.
    if raw.is_empty() || raw.starts_with('/') || Path::new(raw).is_absolute() || is_windows_drive_path(raw) {
        return None;
    }
    if raw.split('/').any(|s| s == "..") {
        return None;
    }
    let parts: Vec<&str> = raw.split('/').filter(|s| !s.is_empty() && *s != ".").collect();
    if parts.is_empty() { None } else { Some(parts.join("/")) }
}

fn is_windows_drive_path(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// "05-icy-tree-crystal-tree.png" -> "Icy tree crystal tree".
fn title_from_file(file: &str) -> String {
    let stem = Path::new(file)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.to_string());
    let without_prefix = strip_numeric_prefix(&stem);
    let spaced = without_prefix.replace('-', " ");
    let collapsed: String = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let base = if collapsed.is_empty() {
        file.to_string()
    } else {
        collapsed
    };
    capitalize(&base)
}

/// Drops a leading `NN-` or `NNa-` (one digit run, an optional single letter, then a dash).
fn strip_numeric_prefix(s: &str) -> &str {
    let digits_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if digits_end == 0 {
        return s;
    }
    let rest = &s[digits_end..];
    let after_letter = if rest.starts_with(|c: char| c.is_ascii_alphabetic()) && rest.len() > 1 {
        &rest[1..]
    } else {
        rest
    };
    after_letter.strip_prefix('-').unwrap_or(s)
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// A PNG's name with its extension (case-insensitively) swapped for `.jpg`.
fn png_to_jpg_name(file: &str) -> String {
    if file.len() >= 4 && file[file.len() - 4..].eq_ignore_ascii_case(".png") {
        format!("{}.jpg", &file[..file.len() - 4])
    } else {
        file.to_string()
    }
}

/// The pictures in shoot order (numeric-aware, as filenames are usually numbered), named from the
/// game's manifest where it names them, else from the file name.
fn list_images(pngs: &[String], manifest: Option<&[ManifestEntry]>) -> Vec<GalleryImage> {
    let by_file: HashMap<&str, &ManifestEntry> = manifest
        .unwrap_or_default()
        .iter()
        .map(|m| (m.file.as_str(), m))
        .collect();
    let mut sorted = pngs.to_vec();
    sorted.sort_by(|a, b| natural_cmp(a, b));
    sorted
        .into_iter()
        .map(|f| {
            let entry = by_file.get(f.as_str());
            let title = entry
                .and_then(|m| m.name.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| title_from_file(&f));
            let group = entry.and_then(|m| m.group.clone()).filter(|g| !g.is_empty());
            GalleryImage {
                file: png_to_jpg_name(&f),
                title,
                group,
            }
        })
        .collect()
}

/// Orders two names the way `localeCompare(..., { numeric: true })` does: a run of digits
/// compares by value, so "2-x" sorts before "10-x".
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        return match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(ca), Some(cb)) if ca.is_ascii_digit() && cb.is_ascii_digit() => {
                let na = take_number(&mut ai);
                let nb = take_number(&mut bi);
                match na.cmp(&nb) {
                    Ordering::Equal => continue,
                    other => other,
                }
            }
            (Some(ca), Some(cb)) => match ca.cmp(&cb) {
                Ordering::Equal => {
                    ai.next();
                    bi.next();
                    continue;
                }
                other => other,
            },
        };
    }
}

fn take_number(it: &mut std::iter::Peekable<std::str::Chars>) -> u64 {
    let mut n: u64 = 0;
    while let Some(&c) = it.peek() {
        if let Some(d) = c.to_digit(10) {
            n = n.saturating_mul(10).saturating_add(d as u64);
            it.next();
        } else {
            break;
        }
    }
    n
}

/// Reads the game's own `index.json` from `out`, if it wrote one and it parses as an array of
/// entries with at least a `file` string; anything else means titles come from file names alone.
fn read_manifest(out: &Path) -> Option<Vec<ManifestEntry>> {
    let text = std::fs::read_to_string(out.join("index.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// PNGs in `out` -> JPEGs (quality 82, at most 1920 px wide, never enlarged) in a fresh folder,
/// swapped in for `dest`'s old set only once every picture and the new `index.json` are written.
fn convert(out: &Path, name: &str, sha: &str, dest: &Path) -> Result<GalleryIndex> {
    let mut pngs = Vec::new();
    if out.exists() {
        for entry in std::fs::read_dir(out).with_context(|| format!("reading {}", out.display()))? {
            let entry = entry?;
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if file_name.to_lowercase().ends_with(".png") {
                pngs.push(file_name);
            }
        }
    }
    if pngs.is_empty() {
        bail!("The command finished but wrote no PNGs to {}.", out.display());
    }
    let manifest = read_manifest(out);
    let images = list_images(&pngs, manifest.as_deref());
    let png_for: HashMap<String, String> = pngs.iter().map(|f| (png_to_jpg_name(f), f.clone())).collect();

    let mut tmp_name = dest.as_os_str().to_os_string();
    tmp_name.push(".new");
    let tmp = PathBuf::from(tmp_name);
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).with_context(|| format!("creating {}", tmp.display()))?;

    for img in &images {
        let png_name = png_for.get(&img.file).expect("every image came from a png in pngs");
        let src = out.join(png_name);
        let decoded = image::open(&src).with_context(|| format!("reading {}", src.display()))?;
        let resized = shrink(decoded);
        let dest_file = tmp.join(&img.file);
        let file = std::fs::File::create(&dest_file).with_context(|| format!("creating {}", dest_file.display()))?;
        let mut encoder = JpegEncoder::new_with_quality(file, JPEG_QUALITY);
        encoder
            .encode_image(&resized.to_rgb8())
            .with_context(|| format!("encoding {}", dest_file.display()))?;
    }

    let idx = GalleryIndex {
        name: name.to_string(),
        shot_at: now_iso8601(),
        sha: sha.to_string(),
        images,
    };
    let index_json = serde_json::to_string_pretty(&idx).context("writing index.json")?;
    std::fs::write(tmp.join("index.json"), index_json).context("writing index.json")?;

    let _ = std::fs::remove_dir_all(dest);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::rename(&tmp, dest).with_context(|| format!("renaming {} to {}", tmp.display(), dest.display()))?;
    Ok(idx)
}

/// Resized to at most [`MAX_WIDTH`] wide, aspect kept, never enlarged.
fn shrink(img: image::DynamicImage) -> image::DynamicImage {
    use image::GenericImageView;
    let (w, h) = img.dimensions();
    if w <= MAX_WIDTH || w == 0 {
        return img;
    }
    let new_h = ((h as u64 * MAX_WIDTH as u64) / w as u64).max(1) as u32;
    img.resize_exact(MAX_WIDTH, new_h, image::imageops::FilterType::Lanczos3)
}

fn now_iso8601() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}
