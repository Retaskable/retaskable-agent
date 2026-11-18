use super::*;
use std::sync::Mutex;

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn build(bytes: &[u8]) -> Build {
    Build {
        name: "gh_x.tar.gz".into(),
        sha256: sha(bytes),
        url: "https://example.test/gh_x.tar.gz".into(),
    }
}

/// An extractor that "unpacks" a top folder holding `bin/gh` (or nothing at all).
fn unpack(with_gh: bool) -> impl Fn(&Path, &Path) -> Result<(), String> {
    move |_, to| {
        if with_gh {
            let bin = to.join("gh_x_linux").join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(bin.join("gh"), "binary").unwrap();
        }
        Ok(())
    }
}

#[test]
fn the_build_is_the_arm_one_on_arm_and_the_64_bit_one_elsewhere() {
    assert_eq!(gh_build("macos", "aarch64").unwrap().name, "gh_2.103.0_macOS_arm64.zip");
    assert_eq!(gh_build("macos", "x86_64").unwrap().name, "gh_2.103.0_macOS_amd64.zip");
    assert_eq!(
        gh_build("windows", "x86_64").unwrap().url,
        "https://github.com/cli/cli/releases/download/v2.103.0/gh_2.103.0_windows_amd64.zip"
    );
    assert_eq!(
        gh_build("linux", "aarch64").unwrap().name,
        "gh_2.103.0_linux_arm64.tar.gz"
    );
    assert!(gh_build("freebsd", "x86_64").is_none());
    assert_eq!(mingit_build("aarch64").name, "MinGit-2.56.0.2-arm64.zip");
    assert_eq!(
        mingit_build("x86_64").url,
        "https://github.com/git-for-windows/git/releases/download/v2.56.0.windows.2/MinGit-2.56.0.2-64-bit.zip"
    );
}

#[test]
fn a_good_download_is_unpacked_into_the_data_folder_and_put_in_use() {
    let data = tempfile::tempdir().unwrap();
    let bytes = b"the archive".to_vec();
    let used = Mutex::new(Vec::new());
    fetch_own_gh(
        data.path(),
        "linux",
        "x86_64",
        Some(build(&bytes)),
        &|_| Ok(bytes.clone()),
        &unpack(true),
        |d| used.lock().unwrap().push(d.to_path_buf()),
    )
    .unwrap();
    let dir = data.path().join("gh");
    assert!(dir.join("bin").join("gh").is_file());
    assert_eq!(*used.lock().unwrap(), vec![dir]);
    // Nothing is left beside it.
    let names: Vec<_> = std::fs::read_dir(data.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["gh"]);
}

#[test]
fn a_bad_checksum_unpacks_nothing() {
    let data = tempfile::tempdir().unwrap();
    let unpacked = Mutex::new(false);
    let err = fetch_own_gh(
        data.path(),
        "linux",
        "x86_64",
        Some(build(b"the real one")),
        &|_| Ok(b"something else".to_vec()),
        &|_, _| {
            *unpacked.lock().unwrap() = true;
            Ok(())
        },
        |_| {},
    )
    .unwrap_err();
    assert_eq!(
        err,
        "the download of gh_x.tar.gz isn't the one GitHub published, so it wasn't used."
    );
    assert!(!*unpacked.lock().unwrap());
    assert_eq!(std::fs::read_dir(data.path()).unwrap().count(), 0);
}

#[test]
fn an_archive_without_the_program_keeps_nothing() {
    let data = tempfile::tempdir().unwrap();
    let bytes = b"x".to_vec();
    let err = fetch_own_gh(
        data.path(),
        "linux",
        "x86_64",
        Some(build(&bytes)),
        &|_| Ok(bytes.clone()),
        &unpack(false),
        |_| {},
    )
    .unwrap_err();
    assert_eq!(err, "gh_2.103.0 has no bin/gh.");
    assert_eq!(std::fs::read_dir(data.path()).unwrap().count(), 0);
}

#[test]
fn a_failed_download_says_why() {
    let data = tempfile::tempdir().unwrap();
    let err = fetch_own_gh(
        data.path(),
        "linux",
        "x86_64",
        Some(build(b"x")),
        &|_| Err("the download answered 404".into()),
        &unpack(true),
        |_| {},
    )
    .unwrap_err();
    assert_eq!(err, "the download answered 404");
}

#[test]
fn mingit_needs_its_cmd_git_exe() {
    let data = tempfile::tempdir().unwrap();
    let bytes = b"mingit".to_vec();
    let b = Build {
        name: "MinGit.zip".into(),
        sha256: sha(&bytes),
        url: "u".into(),
    };
    let err = fetch_own_git(
        data.path(),
        "x86_64",
        Some(b.clone()),
        &|_| Ok(bytes.clone()),
        &|_, _| Ok(()),
    )
    .unwrap_err();
    assert_eq!(err, "MinGit.zip has no cmd\\git.exe.");
    let ok = fetch_own_git(data.path(), "x86_64", Some(b), &|_| Ok(bytes.clone()), &|_, to| {
        std::fs::create_dir_all(to.join("cmd")).unwrap();
        std::fs::write(to.join("cmd").join("git.exe"), "").unwrap();
        Ok(())
    })
    .unwrap();
    assert!(ok.join("cmd").join("git.exe").is_file());
}

#[cfg(unix)]
#[test]
fn tar_is_run_with_an_argument_list() {
    let seen = Mutex::new(None);
    let run = |p: &str, a: &[String], _| {
        *seen.lock().unwrap() = Some((p.to_string(), a.to_vec()));
        Ran {
            code: Some(0),
            ..Ran::default()
        }
    };
    extract_archive(&run, Path::new("/d/a.zip"), Path::new("/d/a.part")).unwrap();
    assert_eq!(
        seen.lock().unwrap().clone().unwrap(),
        (
            "tar".to_string(),
            vec!["-xf".to_string(), "/d/a.zip".into(), "-C".into(), "/d/a.part".into()]
        )
    );
    let fail = |_: &str, _: &[String], _| Ran {
        code: Some(1),
        stderr: "bad archive\n".into(),
        ..Ran::default()
    };
    assert_eq!(
        extract_archive(&fail, Path::new("a"), Path::new("b")).unwrap_err(),
        "couldn't unpack it: bad archive"
    );
}
