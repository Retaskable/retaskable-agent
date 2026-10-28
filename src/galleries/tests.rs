use std::path::Path;

use super::*;
use crate::test_support::{git, repo_with_origin, run};

fn commit_and_push(repo: &Path, file: &str) {
    run(git(repo).args(["add", file]));
    run(git(repo).args(["commit", "--quiet", "-m", file]));
    run(git(repo).args(["push", "--quiet", "origin", "main"]));
}

fn write_test_png(path: &Path, width: u32, height: u32) {
    let img = image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
    });
    image::DynamicImage::ImageRgb8(img).save(path).unwrap();
}

#[cfg(windows)]
fn copy_fixture_command() -> String {
    "mkdir shots && copy fixture.png shots\\01-first.png".to_string()
}
#[cfg(not(windows))]
fn copy_fixture_command() -> String {
    "mkdir -p shots && cp fixture.png shots/01-first.png".to_string()
}

#[test]
fn shoot_runs_the_command_and_converts_its_pictures_into_the_gallery_folder() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    write_test_png(&repo.join("fixture.png"), 3000, 20);
    commit_and_push(&repo, "fixture.png");
    let worktrees = root.path().join("worktrees");
    let dest = root.path().join("dest");
    let gallery = GalleryConfig {
        name: "Models".to_string(),
        command: copy_fixture_command(),
        out_dir: "shots".to_string(),
    };

    let idx = shoot(
        "galleries-shoot-happy-path",
        "models",
        &gallery,
        &repo,
        &worktrees,
        "main",
        &dest,
    )
    .unwrap();

    assert_eq!(idx.name, "Models");
    assert_eq!(idx.images.len(), 1);
    assert_eq!(idx.images[0].file, "01-first.jpg");
    assert_eq!(idx.images[0].title, "First");
    assert!(dest.join("01-first.jpg").exists());
    assert!(dest.join("index.json").exists());
    // Shrunk from 3000 px wide to the 1920 px cap.
    assert_eq!(image::open(dest.join("01-first.jpg")).unwrap().width(), 1920);
    assert_eq!(status("galleries-shoot-happy-path", "models"), GalleryStatus::Idle);
    assert!(!busy("galleries-shoot-happy-path"));
}

#[test]
fn shoot_marks_the_gallery_failed_when_the_command_writes_no_pictures() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    let worktrees = root.path().join("worktrees");
    let dest = root.path().join("dest");
    let empty_command = if cfg!(windows) { "exit 0" } else { "true" };
    let gallery = GalleryConfig {
        name: "Models".to_string(),
        command: empty_command.to_string(),
        out_dir: "shots".to_string(),
    };

    let err = shoot(
        "galleries-shoot-no-pictures",
        "models",
        &gallery,
        &repo,
        &worktrees,
        "main",
        &dest,
    )
    .unwrap_err();

    assert!(err.to_string().contains("wrote no PNGs"), "{err}");
    assert!(matches!(
        status("galleries-shoot-no-pictures", "models"),
        GalleryStatus::Failed(_)
    ));
    assert!(!busy("galleries-shoot-no-pictures"));
}

#[test]
fn shoot_fails_when_the_command_exits_non_zero() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    let worktrees = root.path().join("worktrees");
    let dest = root.path().join("dest");
    let gallery = GalleryConfig {
        name: "Models".to_string(),
        command: "exit 3".to_string(),
        out_dir: "shots".to_string(),
    };

    let err = shoot(
        "galleries-shoot-failing-command",
        "models",
        &gallery,
        &repo,
        &worktrees,
        "main",
        &dest,
    )
    .unwrap_err();

    assert!(err.to_string().contains("failed (3)"), "{err}");
}

#[test]
fn status_and_busy_default_before_any_shoot() {
    assert_eq!(status("galleries-shoot-never-shot", "models"), GalleryStatus::Idle);
    assert!(!busy("galleries-shoot-never-shot"));
}

#[test]
fn shoot_refusal_reports_no_command_set() {
    let gallery = GalleryConfig {
        name: "Models".to_string(),
        command: "  ".to_string(),
        out_dir: "out".to_string(),
    };
    assert_eq!(
        shoot_refusal("galleries-shoot-refusal-a", &gallery),
        Some("No command set for this gallery.".to_string())
    );
}

#[test]
fn shoot_refusal_reports_an_unsafe_out_dir() {
    let gallery = GalleryConfig {
        name: "Models".to_string(),
        command: "echo hi".to_string(),
        out_dir: "../outside".to_string(),
    };
    assert_eq!(
        shoot_refusal("galleries-shoot-refusal-b", &gallery),
        Some("The output folder must be inside the repo: ../outside".to_string())
    );
}

#[test]
fn shoot_refusal_reports_already_shooting() {
    let gallery = GalleryConfig {
        name: "Models".to_string(),
        command: "echo hi".to_string(),
        out_dir: "out".to_string(),
    };
    SHOOTING
        .lock()
        .unwrap()
        .insert("galleries-shoot-refusal-c".to_string(), "Other".to_string());
    let refusal = shoot_refusal("galleries-shoot-refusal-c", &gallery);
    SHOOTING.lock().unwrap().remove("galleries-shoot-refusal-c");
    assert_eq!(refusal, Some("Already shooting.".to_string()));
}

#[test]
fn safe_out_dir_accepts_an_ordinary_relative_path_and_normalizes_slashes() {
    assert_eq!(safe_out_dir("target\\models"), Some("target/models".to_string()));
    assert_eq!(safe_out_dir("  ./target/models/  "), Some("target/models".to_string()));
}

#[test]
fn safe_out_dir_refuses_anything_that_leaves_the_repo_or_is_the_repo_itself() {
    assert_eq!(safe_out_dir(""), None);
    assert_eq!(safe_out_dir("."), None);
    assert_eq!(safe_out_dir("/etc/passwd"), None);
    assert_eq!(safe_out_dir("C:/Windows"), None);
    assert_eq!(safe_out_dir("c:\\Windows"), None);
    assert_eq!(safe_out_dir("target/../../../etc"), None);
}

#[test]
fn title_from_file_strips_a_numbered_prefix_and_dashes() {
    assert_eq!(title_from_file("05-icy-tree-crystal-tree.png"), "Icy tree crystal tree");
    assert_eq!(title_from_file("12a-second.png"), "Second");
    assert_eq!(title_from_file("plain.png"), "Plain");
    assert_eq!(title_from_file("05-.png"), "05-.png");
}

#[test]
fn list_images_sorts_numerically_and_prefers_the_manifests_name_and_group() {
    let pngs = vec!["10-b.png".to_string(), "2-a.png".to_string(), "1-c.png".to_string()];
    let manifest = vec![ManifestEntry {
        file: "2-a.png".to_string(),
        name: Some("Custom Name".to_string()),
        group: Some("Group A".to_string()),
    }];

    let images = list_images(&pngs, Some(&manifest));

    assert_eq!(
        images.iter().map(|i| i.file.as_str()).collect::<Vec<_>>(),
        vec!["1-c.jpg", "2-a.jpg", "10-b.jpg"]
    );
    assert_eq!(images[0].title, "C");
    assert_eq!(images[1].title, "Custom Name");
    assert_eq!(images[1].group, Some("Group A".to_string()));
    assert_eq!(images[2].title, "B");
    assert_eq!(images[2].group, None);
}

#[test]
fn convert_writes_jpegs_and_an_index_json_then_swaps_them_into_dest() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    write_test_png(&out.join("01-first.png"), 10, 10);
    write_test_png(&out.join("02-second.png"), 10, 10);
    let dest = root.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("stale.jpg"), "old").unwrap();

    let idx = convert(&out, "Models", "deadbeef", &dest).unwrap();

    assert_eq!(idx.sha, "deadbeef");
    assert_eq!(idx.images.len(), 2);
    assert!(dest.join("01-first.jpg").exists());
    assert!(dest.join("02-second.jpg").exists());
    assert!(!dest.join("stale.jpg").exists(), "the old set should be replaced");
    assert!(!dest.with_file_name("dest.new").exists());
}

#[test]
fn convert_fails_when_the_command_wrote_no_pngs() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let dest = root.path().join("dest");

    let err = convert(&out, "Models", "deadbeef", &dest).unwrap_err();

    assert!(err.to_string().contains("wrote no PNGs"), "{err}");
}

#[test]
fn swap_in_keeps_the_old_set_when_the_final_rename_fails() {
    let root = tempfile::tempdir().unwrap();
    let dest = root.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("old.jpg"), "old").unwrap();
    // Never created, so the rename into `dest` below is guaranteed to fail.
    let tmp = root.path().join("dest.new");

    let err = swap_in(&tmp, &dest).unwrap_err();

    assert!(err.to_string().contains("renaming"), "{err}");
    assert!(
        dest.join("old.jpg").exists(),
        "the old set must survive a failed swap, not be deleted before the swap is attempted"
    );
}

#[test]
fn valid_gallery_slug_accepts_word_chars_and_dashes_only() {
    assert!(valid_gallery_slug("models"));
    assert!(valid_gallery_slug("my_gallery-2"));
    assert!(!valid_gallery_slug(""));
    assert!(!valid_gallery_slug("has.dot"));
    assert!(!valid_gallery_slug("has/slash"));
    assert!(!valid_gallery_slug("has space"));
}

#[test]
fn valid_picture_file_accepts_word_chars_dots_and_dashes_ending_in_dot_jpg() {
    assert!(valid_picture_file("01-first.jpg"));
    assert!(valid_picture_file("01-FIRST.JPG"));
    assert!(!valid_picture_file(""));
    assert!(!valid_picture_file("picture.png"));
    assert!(!valid_picture_file("../escape.jpg"));
    assert!(!valid_picture_file("a/b.jpg"));
    assert!(!valid_picture_file(&format!("{}.jpg", "a".repeat(100))));
}

#[test]
fn gallery_index_reads_the_last_shoots_index_json() {
    let root = tempfile::tempdir().unwrap();
    let dest = root
        .path()
        .join("projects")
        .join("demo")
        .join("galleries")
        .join("models");
    std::fs::create_dir_all(&dest).unwrap();
    let idx = GalleryIndex {
        name: "Models".to_string(),
        shot_at: "2026-01-01T00:00:00Z".to_string(),
        sha: "deadbeef".to_string(),
        images: vec![GalleryImage {
            file: "01-first.jpg".to_string(),
            title: "First".to_string(),
            group: None,
        }],
    };
    std::fs::write(dest.join("index.json"), serde_json::to_string(&idx).unwrap()).unwrap();

    let found = gallery_index(root.path(), "demo", "models").unwrap();

    assert_eq!(found, idx);
}

#[test]
fn gallery_index_refuses_an_invalid_or_unknown_slug() {
    let root = tempfile::tempdir().unwrap();

    assert_eq!(
        gallery_index(root.path(), "demo", "has space"),
        Err("No gallery called has space.".to_string())
    );
    assert_eq!(
        gallery_index(root.path(), "demo", "never-shot"),
        Err("No gallery called never-shot.".to_string())
    );
}

fn write_test_gallery(data_dir: &Path, project: &str, slug: &str, file: &str, width: u32, height: u32) -> GalleryIndex {
    let dest = paths::gallery_dir(data_dir, project, slug);
    std::fs::create_dir_all(&dest).unwrap();
    let img = image::RgbImage::from_fn(width, height, |_, _| image::Rgb(rand::random::<[u8; 3]>()));
    let out = std::fs::File::create(dest.join(file)).unwrap();
    let mut encoder = JpegEncoder::new_with_quality(out, JPEG_QUALITY);
    encoder.encode_image(&img).unwrap();
    let idx = GalleryIndex {
        name: "Models".to_string(),
        shot_at: "2026-01-01T00:00:00Z".to_string(),
        sha: "deadbeef".to_string(),
        images: vec![GalleryImage {
            file: file.to_string(),
            title: "First".to_string(),
            group: None,
        }],
    };
    std::fs::write(dest.join("index.json"), serde_json::to_string(&idx).unwrap()).unwrap();
    idx
}

#[test]
fn gallery_image_returns_a_small_picture_as_is() {
    let root = tempfile::tempdir().unwrap();
    write_test_gallery(root.path(), "demo", "models", "01-first.jpg", 10, 10);

    let answer = gallery_image(root.path(), "demo", "models", "01-first.jpg").unwrap();

    assert_eq!(answer.file, "01-first.jpg");
    assert_eq!(answer.kind, "image/jpeg");
    assert_eq!(answer.shot_at, "2026-01-01T00:00:00Z");
    let bytes = base64.decode(&answer.data).unwrap();
    assert_eq!(
        bytes,
        std::fs::read(paths::gallery_dir(root.path(), "demo", "models").join("01-first.jpg")).unwrap()
    );
}

#[test]
fn gallery_image_refuses_an_invalid_file_name_or_one_not_in_the_index() {
    let root = tempfile::tempdir().unwrap();
    write_test_gallery(root.path(), "demo", "models", "01-first.jpg", 10, 10);

    assert_eq!(
        gallery_image(root.path(), "demo", "models", "../escape.jpg"),
        Err("No picture ../escape.jpg in that gallery.".to_string())
    );
    assert_eq!(
        gallery_image(root.path(), "demo", "models", "02-second.jpg"),
        Err("No picture 02-second.jpg in that gallery.".to_string())
    );
}

#[test]
fn gallery_image_shrinks_a_picture_too_big_to_send_and_caches_the_result() {
    let root = tempfile::tempdir().unwrap();
    // Random noise compresses poorly, so a 1920-wide shot of it is reliably over the cap.
    write_test_gallery(root.path(), "demo", "resend", "01-first.jpg", 1920, 1080);

    let answer = gallery_image(root.path(), "demo", "resend", "01-first.jpg").unwrap();

    assert!(answer.data.len() <= MAX_PICTURE_BASE64, "{}", answer.data.len());
    let decoded = base64.decode(&answer.data).unwrap();
    let resized = image::load_from_memory(&decoded).unwrap();
    assert!(resized.width() <= RESEND_WIDTHS[0]);
    let key: PictureKey = (
        "demo".to_string(),
        "resend".to_string(),
        "01-first.jpg".to_string(),
        "2026-01-01T00:00:00Z".to_string(),
    );
    assert_eq!(cached_picture(&key), Some(answer.data.clone()));

    // Served again from the cache, without needing to re-decode or re-encode the original.
    let again = gallery_image(root.path(), "demo", "resend", "01-first.jpg").unwrap();
    assert_eq!(again.data, answer.data);
}

#[test]
fn picture_cache_overwrites_rather_than_duplicates_an_existing_key() {
    let key: PictureKey = (
        "overwrite-test".to_string(),
        "g".to_string(),
        "f.jpg".to_string(),
        "t".to_string(),
    );
    cache_picture(key.clone(), "first".to_string());
    cache_picture(key.clone(), "second".to_string());

    assert_eq!(cached_picture(&key), Some("second".to_string()));
}

#[test]
fn picture_cache_evicts_the_oldest_entry_once_full() {
    // A generous margin over the cap: enough that even a handful of other tests' entries ahead
    // of ours in the queue can't stop our own earliest key from being evicted.
    let total = MAX_CACHED_PICTURES * 3;
    for i in 0..total {
        cache_picture(
            (
                "evict-test".to_string(),
                "gallery".to_string(),
                format!("{i}.jpg"),
                "t".to_string(),
            ),
            "data".to_string(),
        );
    }

    let first_key: PictureKey = (
        "evict-test".to_string(),
        "gallery".to_string(),
        "0.jpg".to_string(),
        "t".to_string(),
    );
    let last_key: PictureKey = (
        "evict-test".to_string(),
        "gallery".to_string(),
        format!("{}.jpg", total - 1),
        "t".to_string(),
    );
    assert_eq!(
        cached_picture(&first_key),
        None,
        "the oldest entry should have been evicted"
    );
    assert_eq!(cached_picture(&last_key), Some("data".to_string()));
}

#[test]
fn needs_npm_install_is_false_without_a_package_json() {
    let root = tempfile::tempdir().unwrap();
    assert!(!needs_npm_install(root.path()));
}

#[test]
fn needs_npm_install_is_true_when_node_modules_is_missing() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("package.json"), "{}").unwrap();
    assert!(needs_npm_install(root.path()));
}

#[test]
fn needs_npm_install_is_false_once_node_modules_is_up_to_date() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("package.json"), "{}").unwrap();
    std::fs::write(root.path().join("package-lock.json"), "{}").unwrap();
    std::fs::create_dir_all(root.path().join("node_modules")).unwrap();
    std::fs::write(root.path().join("node_modules").join(".package-lock.json"), "{}").unwrap();
    let now = std::time::SystemTime::now();
    set_mtime(
        &root.path().join("package-lock.json"),
        now - std::time::Duration::from_secs(60),
    );
    set_mtime(&root.path().join("node_modules").join(".package-lock.json"), now);

    assert!(!needs_npm_install(root.path()));
}

fn set_mtime(path: &Path, when: std::time::SystemTime) {
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

#[test]
fn needs_npm_install_is_true_once_the_lockfile_is_newer_than_the_installed_copy() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("package.json"), "{}").unwrap();
    std::fs::write(root.path().join("package-lock.json"), "{}").unwrap();
    std::fs::create_dir_all(root.path().join("node_modules")).unwrap();
    std::fs::write(root.path().join("node_modules").join(".package-lock.json"), "{}").unwrap();
    let now = std::time::SystemTime::now();
    set_mtime(
        &root.path().join("node_modules").join(".package-lock.json"),
        now - std::time::Duration::from_secs(60),
    );
    set_mtime(&root.path().join("package-lock.json"), now);

    assert!(needs_npm_install(root.path()));
}

#[test]
fn npm_install_command_prefers_ci_when_theres_a_lockfile() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("package-lock.json"), "{}").unwrap();
    assert_eq!(npm_install_command(root.path()), "npm ci");
}

#[test]
fn npm_install_command_falls_back_to_install_without_a_lockfile() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(npm_install_command(root.path()), "npm install");
}

#[test]
fn npm_install_if_needed_with_skips_the_runner_when_nothing_is_needed() {
    let root = tempfile::tempdir().unwrap();
    let ran = std::cell::Cell::new(false);

    npm_install_if_needed_with(root.path(), |_, _, _| {
        ran.set(true);
        Ok((0, String::new(), false))
    })
    .unwrap();

    assert!(!ran.get());
}

#[test]
fn npm_install_if_needed_with_runs_npm_ci_and_passes_on_its_failure() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("package.json"), "{}").unwrap();
    std::fs::write(root.path().join("package-lock.json"), "{}").unwrap();
    let seen = std::cell::RefCell::new(None);

    let err = npm_install_if_needed_with(root.path(), |command, cwd, timeout| {
        *seen.borrow_mut() = Some((command.to_string(), cwd.to_path_buf(), timeout));
        Ok((1, "boom".to_string(), false))
    })
    .unwrap_err();

    assert_eq!(seen.borrow().as_ref().unwrap().0, "npm ci");
    assert_eq!(seen.borrow().as_ref().unwrap().1, root.path());
    assert_eq!(seen.borrow().as_ref().unwrap().2, NPM_INSTALL_TIMEOUT);
    assert!(err.to_string().contains("failed (1)"), "{err}");
    assert!(err.to_string().contains("boom"), "{err}");
}

#[test]
fn scrub_path_drops_every_node_modules_bin_and_puts_this_checkouts_first() {
    let dir = Path::new("checkout");
    let other_sep = if cfg!(windows) { ';' } else { ':' };
    let other = std::env::join_paths([
        Path::new("checkout").join("node_modules").join(".bin"),
        Path::new("other-checkout").join("node_modules").join(".bin"),
        Path::new("usr").join("bin"),
    ])
    .unwrap()
    .to_string_lossy()
    .into_owned();

    let scrubbed = scrub_path(&other, dir);

    let parts: Vec<&str> = scrubbed.split(other_sep).collect();
    assert_eq!(parts[0], dir.join("node_modules").join(".bin").to_string_lossy());
    assert_eq!(parts.len(), 2, "{scrubbed}");
    assert!(parts[1].ends_with("bin"), "{scrubbed}");
}
