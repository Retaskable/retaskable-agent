use super::*;

fn sig(valid: bool, signer: Option<&str>) -> Signature {
    Signature {
        valid,
        signer: signer.map(str::to_string),
    }
}

#[test]
fn an_unsigned_running_copy_has_nothing_to_compare_with() {
    assert_eq!(signature_refusal(&sig(false, None), &sig(false, None)), None);
    assert_eq!(
        signature_refusal(&sig(false, None), &sig(true, Some("CN=Mallory"))),
        None
    );
}

#[test]
fn a_validly_signed_running_copy_refuses_an_unsigned_download() {
    let reason = signature_refusal(&sig(true, Some("CN=Retaskable")), &sig(false, None)).unwrap();
    assert_eq!(reason, "the download was refused: it isn't signed, and this copy is");
}

#[test]
fn a_validly_signed_running_copy_refuses_a_download_signed_by_someone_else() {
    let reason = signature_refusal(&sig(true, Some("CN=Retaskable")), &sig(true, Some("CN=Mallory"))).unwrap();
    assert_eq!(
        reason,
        "the download was refused: it's signed by CN=Mallory, not CN=Retaskable as this copy is"
    );
}

#[test]
fn a_validly_signed_running_copy_accepts_a_download_signed_the_same() {
    assert_eq!(
        signature_refusal(&sig(true, Some("CN=Retaskable")), &sig(true, Some("CN=Retaskable"))),
        None
    );
}

#[test]
fn authenticode_status_and_signer_subject_are_the_powershell_scripts_own_two_lines() {
    assert_eq!(
        parse_authenticode_signature("Valid\nCN=Retaskable, O=Retaskable\n"),
        sig(true, Some("CN=Retaskable, O=Retaskable"))
    );
    assert_eq!(parse_authenticode_signature("NotSigned\n\n"), sig(false, None));
    assert_eq!(parse_authenticode_signature(""), sig(false, None));
    // A signer line is only trusted once the status itself says Valid.
    assert_eq!(
        parse_authenticode_signature("HashMismatch\nCN=Mallory\n"),
        sig(false, None)
    );
}

#[cfg(target_os = "windows")]
mod windows {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn an_unsigned_running_copy_prepares_an_unsigned_download_without_asking_powershell_to_refuse() {
        let dir = TempDir::new().unwrap();
        let running = dir.path().join("running.exe");
        let downloaded = dir.path().join("downloaded.exe");
        fs::write(&running, b"not a real pe").unwrap();
        fs::write(&downloaded, b"also not a real pe").unwrap();
        let installer = WindowsInstaller::new(running);
        let prepared = installer.prepare(&downloaded).unwrap();
        assert_eq!(prepared, downloaded);
    }

    #[test]
    fn a_plain_file_has_no_authenticode_signature() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("plain.exe");
        fs::write(&file, b"not a real pe").unwrap();
        let signature = authenticode_signature(&file).unwrap();
        assert!(!signature.valid);
        assert_eq!(signature.signer, None);
    }

    #[test]
    fn installing_a_prepared_file_that_doesnt_exist_errors_instead_of_panicking() {
        let dir = TempDir::new().unwrap();
        let installer = WindowsInstaller::new(dir.path().join("running.exe"));
        let err = installer.install(&dir.path().join("missing.exe")).unwrap_err();
        assert!(err.contains("missing.exe"));
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;

    #[test]
    fn a_copy_not_running_from_an_app_cant_replace_itself() {
        let installer = MacInstaller::new(PathBuf::from("/usr/local/bin/retaskable"));
        assert_eq!(installer.cannot(), Some("it isn't running from an app".to_string()));
    }

    #[test]
    fn a_copy_running_from_an_app_can_replace_itself() {
        let installer = MacInstaller::new(PathBuf::from("/Applications/Retaskable.app/Contents/MacOS/retaskable"));
        assert_eq!(installer.cannot(), None);
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod linux {
    use super::*;

    #[test]
    fn a_copy_with_no_appimage_env_cant_replace_itself() {
        let installer = LinuxInstaller::new(None);
        assert_eq!(installer.cannot(), Some("it isn't running as an AppImage".to_string()));
    }

    #[test]
    fn a_copy_running_as_an_appimage_can_replace_itself() {
        let installer = LinuxInstaller::new(Some(PathBuf::from("/tmp/Retaskable.AppImage")));
        assert_eq!(installer.cannot(), None);
    }
}
