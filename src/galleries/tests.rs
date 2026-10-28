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
