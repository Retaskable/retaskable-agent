//! Galleries (docs/specs/agent/06.8-galleries-the-game-photographs-itself.md,
//! docs/specs/tray/05.3-private-galleries-and-pictures-from.md): a gallery's `command` puts the
//! game into every subject in turn, screenshots it, writes PNGs to `outDir` and exits; `shoot`
//! runs that command on the project's base checkout and converts what it wrote into small JPEGs
//! kept at `<dataDir>/projects/<project>/galleries/<slug>/`, replacing the old set only once the
//! new one is all written.
//!
//! Before the command runs, a JS-based game's `node_modules` is installed if it's missing or
//! `package-lock.json` is newer than it, and every `node_modules/.bin` already on `PATH` is
//! dropped so the checkout's own comes first (06.8, bullets 9-10).
//!
//! This also answers `gallery_index`/`gallery_image` (tray.md 5.3.2) by reading a gallery's last
//! shoot straight off disk: `gallery_image` shrinks a picture further and caches the result in
//! memory when its base64 would otherwise be over 768 KiB. Per tray.md 5.3.1, reading the machine
//! like this is really the fallback for a gallery not yet uploaded to the backlog store (a
//! `/demo` environment); the primary path, the store serving a shot gallery once it's uploaded,
//! needs that upload, which doesn't exist yet. Neither is wired to the hub either way, since that
//! needs the command-dispatch layer this agent doesn't have for any command yet.
//!
//! This is the shoot-and-shrink pipeline on its own: it runs the command, converts its pictures,
//! and tracks a gallery's status in memory. Still to come, once there's something to wire it to:
//! uploading a shot gallery's set to the backlog store, refusing a shoot while the baseline, Play
//! main or a sync holds the base checkout (none of which exist yet either), and the settings page
//! that lets someone set a gallery's command.
#[cfg(test)]
mod tests;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as base64;
use image::codecs::jpeg::JpegEncoder;
use serde::{Deserialize, Serialize};

use crate::config::GalleryConfig;
use crate::lanes;
use crate::paths;
use crate::proc;
use crate::sync::lock;

const SHOOT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const JPEG_QUALITY: u8 = 82;
const MAX_WIDTH: u32 = 1920;
const TAIL_LINES: usize = 40;
/// A picture sent as-is above this many base64 bytes is re-encoded smaller instead (tray.md
/// 5.3.2: "768 KiB").
const MAX_PICTURE_BASE64: usize = 768 * 1024;
/// Narrower widths tried in order until a picture's base64 fits under [`MAX_PICTURE_BASE64`].
const RESEND_WIDTHS: [u32; 2] = [1280, 960];
const RESEND_QUALITY: u8 = 75;
/// How many re-encoded pictures are kept in memory at once, oldest evicted first.
const MAX_CACHED_PICTURES: usize = 50;

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

/// A `gallery_image` answer (tray.md 5.3.2): passed through, never kept beyond the in-memory
/// cache of re-encoded pictures.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PictureAnswer {
    pub file: String,
    pub shot_at: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub data: String,
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

/// Project, slug, file and the gallery's `shotAt` when it was re-encoded, so a reshoot of the
/// same file name naturally misses the cache instead of serving a stale picture.
type PictureKey = (String, String, String, String);
/// Re-encoded pictures only (tray.md 5.3.2): one that already fits isn't worth caching. Oldest
/// evicted first once there are more than [`MAX_CACHED_PICTURES`].
static PICTURE_CACHE: LazyLock<Mutex<VecDeque<(PictureKey, String)>>> = LazyLock::new(|| Mutex::new(VecDeque::new()));

fn cached_picture(key: &PictureKey) -> Option<String> {
    lock(&PICTURE_CACHE)
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, data)| data.clone())
}

fn cache_picture(key: PictureKey, data: String) {
    let mut cache = lock(&PICTURE_CACHE);
    cache.retain(|(k, _)| k != &key);
    cache.push_back((key, data));
    while cache.len() > MAX_CACHED_PICTURES {
        cache.pop_front();
    }
}

fn set_status(project: &str, slug: &str, status: GalleryStatus) {
    lock(&STATUS).insert((project.to_string(), slug.to_string()), status);
}

/// A gallery's current status, `idle` before its first shoot.
pub fn status(project: &str, slug: &str) -> GalleryStatus {
    lock(&STATUS)
        .get(&(project.to_string(), slug.to_string()))
        .cloned()
        .unwrap_or_default()
}

/// True while a shoot holds this project's base checkout.
pub fn busy(project: &str) -> bool {
    lock(&SHOOTING).contains_key(project)
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

/// A gallery slug, as the hub sends it: `/^[\w-]+$/`.
fn valid_gallery_slug(slug: &str) -> bool {
    !slug.is_empty() && slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A picture file name, as the hub sends it (tray.md 5.3.2, `PICTURE_FILE`):
/// `/^[\w.-]{1,100}\.jpg$/i`. No `/` or `\` can match, so joining it to a gallery's directory can
/// never leave it.
fn valid_picture_file(file: &str) -> bool {
    let len = file.chars().count();
    (1..=100).contains(&len)
        && file.to_ascii_lowercase().ends_with(".jpg")
        && file
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
}

/// `gallery_index { gallery }` (tray.md 5.3.2): the gallery's last shoot, read straight off disk.
pub fn gallery_index(data_dir: &Path, project: &str, slug: &str) -> Result<GalleryIndex, String> {
    if !valid_gallery_slug(slug) {
        return Err(format!("No gallery called {slug}."));
    }
    let index_path = paths::gallery_dir(data_dir, project, slug).join("index.json");
    let text = std::fs::read_to_string(&index_path).map_err(|_| format!("No gallery called {slug}."))?;
    serde_json::from_str(&text).map_err(|_| format!("No gallery called {slug}."))
}

/// `gallery_image { gallery, file }` (tray.md 5.3.2): one picture from the gallery's last shoot,
/// base64-encoded, re-encoded smaller and cached in memory first if it would otherwise be over
/// [`MAX_PICTURE_BASE64`].
pub fn gallery_image(data_dir: &Path, project: &str, slug: &str, file: &str) -> Result<PictureAnswer, String> {
    if !valid_picture_file(file) {
        return Err(format!("No picture {file} in that gallery."));
    }
    let idx = gallery_index(data_dir, project, slug)?;
    if !idx.images.iter().any(|img| img.file == file) {
        return Err(format!("No picture {file} in that gallery."));
    }
    let key: PictureKey = (
        project.to_string(),
        slug.to_string(),
        file.to_string(),
        idx.shot_at.clone(),
    );
    if let Some(data) = cached_picture(&key) {
        return Ok(PictureAnswer {
            file: file.to_string(),
            shot_at: idx.shot_at,
            kind: "image/jpeg",
            data,
        });
    }
    let path = paths::gallery_dir(data_dir, project, slug).join(file);
    let bytes = std::fs::read(&path).map_err(|_| format!("No picture {file} in that gallery."))?;
    // Base64 inflates length to `ceil(n / 3) * 4`, checked before actually encoding.
    let base64_len = bytes.len().div_ceil(3) * 4;
    if base64_len <= MAX_PICTURE_BASE64 {
        return Ok(PictureAnswer {
            file: file.to_string(),
            shot_at: idx.shot_at,
            kind: "image/jpeg",
            data: base64.encode(&bytes),
        });
    }
    let decoded = image::load_from_memory(&bytes).map_err(|_| "Too big to send from the machine.".to_string())?;
    for width in RESEND_WIDTHS {
        let resized = shrink_to(decoded.clone(), width);
        let data = encode_jpeg_base64(&resized, RESEND_QUALITY)
            .map_err(|_| "Too big to send from the machine.".to_string())?;
        if data.len() <= MAX_PICTURE_BASE64 {
            cache_picture(key, data.clone());
            return Ok(PictureAnswer {
                file: file.to_string(),
                shot_at: idx.shot_at,
                kind: "image/jpeg",
                data,
            });
        }
    }
    Err("Too big to send from the machine.".to_string())
}

fn encode_jpeg_base64(img: &image::DynamicImage, quality: u8) -> Result<String> {
    let mut bytes = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut bytes, quality);
    encoder.encode_image(&img.to_rgb8())?;
    Ok(base64.encode(&bytes))
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
    lock(&SHOOTING).insert(project.to_string(), gallery.name.clone());
    set_status(project, slug, GalleryStatus::Shooting);
    let result = run_shoot(&ShootContext {
        gallery,
        out_dir: &out_dir,
        repo_path,
        worktrees_dir,
        base_branch,
        slug,
        project,
        gallery_dest,
    });
    lock(&SHOOTING).remove(project);
    match &result {
        Ok(_) => set_status(project, slug, GalleryStatus::Idle),
        Err(e) => set_status(project, slug, GalleryStatus::Failed(e.to_string())),
    }
    result
}

/// Everything [`run_shoot`] needs, bundled so it takes one argument instead of eight.
struct ShootContext<'a> {
    gallery: &'a GalleryConfig,
    out_dir: &'a str,
    repo_path: &'a Path,
    worktrees_dir: &'a Path,
    base_branch: &'a str,
    slug: &'a str,
    project: &'a str,
    gallery_dest: &'a Path,
}

fn run_shoot(ctx: &ShootContext) -> Result<GalleryIndex> {
    let dir = lanes::prepare_base_checkout(ctx.repo_path, ctx.worktrees_dir, ctx.base_branch)
        .context("preparing the base checkout")?;
    let sha = lanes::head_sha(&dir).context("reading the base checkout's commit")?;
    let out = dir.join(ctx.out_dir);
    let _ = std::fs::remove_dir_all(&out);
    npm_install_if_needed(&dir)?;
    let (code, output, timed_out) = run_command(&ctx.gallery.command, &dir)?;
    if code != 0 || timed_out {
        let what = if timed_out {
            "timed out".to_string()
        } else {
            format!("failed ({code})")
        };
        bail!(
            "`{}` {what}:\n{}",
            ctx.gallery.command,
            proc::tail_lines(&output, TAIL_LINES)
        );
    }
    set_status(ctx.project, ctx.slug, GalleryStatus::Converting);
    convert(&out, &ctx.gallery.name, &sha, ctx.gallery_dest)
}

/// Installs a JS-based game's dependencies first when they're missing or stale (06.8 bullet 9),
/// using the real `PATH` and a real `npm` subprocess.
fn npm_install_if_needed(dir: &Path) -> Result<()> {
    npm_install_if_needed_with(dir, proc::run_command_with_timeout)
}

/// [`npm_install_if_needed`] with the command runner injected, so tests don't need a real `npm`.
fn npm_install_if_needed_with(
    dir: &Path,
    run: impl Fn(&str, &Path, Duration) -> Result<(i32, String, bool)>,
) -> Result<()> {
    if !proc::needs_npm_install(dir) {
        return Ok(());
    }
    let command = proc::npm_install_command(dir);
    let (code, output, timed_out) = run(command, dir, proc::NPM_INSTALL_TIMEOUT)?;
    if code != 0 || timed_out {
        let what = if timed_out {
            "timed out".to_string()
        } else {
            format!("failed ({code})")
        };
        bail!("`{command}` {what}:\n{}", proc::tail_lines(&output, TAIL_LINES));
    }
    Ok(())
}

/// Runs `command` in a shell under `cwd`, with a window (it may need to render) and no input,
/// for up to [`SHOOT_TIMEOUT`]. Returns its exit code (-1 if it couldn't be read), combined
/// stdout/stderr (capped), and whether it was killed for running too long.
fn run_command(command: &str, cwd: &Path) -> Result<(i32, String, bool)> {
    proc::run_command_with_timeout(command, cwd, SHOOT_TIMEOUT)
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
        let resized = shrink_to(decoded, MAX_WIDTH);
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

    swap_in(&tmp, dest)?;
    Ok(idx)
}

/// Puts `tmp` at `dest`, keeping `dest`'s previous contents in place until `tmp` has fully taken
/// over: `dest` is moved aside rather than deleted, so a failure partway through (the rename
/// itself, not just a crash before it) leaves the old set recoverable at `dest` instead of gone
/// with nothing to replace it.
fn swap_in(tmp: &Path, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut old_name = dest.as_os_str().to_os_string();
    old_name.push(".old");
    let old = PathBuf::from(old_name);
    let _ = std::fs::remove_dir_all(&old);
    let had_old = dest.exists() && std::fs::rename(dest, &old).is_ok();

    match std::fs::rename(tmp, dest) {
        Ok(()) => {
            if had_old {
                let _ = std::fs::remove_dir_all(&old);
            }
            Ok(())
        }
        Err(e) => {
            if had_old {
                let _ = std::fs::rename(&old, dest);
            }
            Err(e).with_context(|| format!("renaming {} to {}", tmp.display(), dest.display()))
        }
    }
}

/// Resized to at most `max_width` wide, aspect kept, never enlarged.
fn shrink_to(img: image::DynamicImage, max_width: u32) -> image::DynamicImage {
    use image::GenericImageView;
    let (w, h) = img.dimensions();
    if w <= max_width || w == 0 {
        return img;
    }
    let new_h = ((h as u64 * max_width as u64) / w as u64).max(1) as u32;
    img.resize_exact(max_width, new_h, image::imageops::FilterType::Lanczos3)
}

fn now_iso8601() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}
