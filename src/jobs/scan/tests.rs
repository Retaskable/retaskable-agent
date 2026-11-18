use super::*;

fn settings(staging: &str) -> ScanSettings {
    ScanSettings {
        staging_url: Some(staging.to_string()),
        ..Default::default()
    }
}

#[test]
fn the_staging_origin_or_why_not() {
    let (origin, url) = settings(" https://Stage.example/a/b ")
        .staging_origin(&PROBES_WORDS)
        .unwrap();
    assert_eq!(
        (origin.as_str(), url.host_str()),
        ("https://stage.example", Some("stage.example"))
    );
    assert_eq!(
        settings("").staging_origin(&PROBES_WORDS).unwrap_err(),
        "The probes need a staging url (deploy.staging.url) to run on."
    );
    assert_eq!(
        ScanSettings::default().staging_origin(&A11Y_WORDS).unwrap_err(),
        "The accessibility sweep needs a staging url (deploy.staging.url) to run on."
    );
    assert_eq!(
        settings("nope").staging_origin(&PROBES_WORDS).unwrap_err(),
        "The probes can't run on the staging url nope: it isn't a url."
    );
    assert!(settings("ftp://x.example").staging_origin(&PROBES_WORDS).is_err());
}

#[test]
fn production_is_never_scanned() {
    let mut s = settings("https://app.example/stage");
    s.production_url = Some("https://app.example".to_string());
    assert_eq!(
        s.staging_origin(&PROBES_WORDS).unwrap_err(),
        "The probes run only on staging, and the staging url is production's."
    );
    assert_eq!(
        s.staging_origin(&A11Y_WORDS).unwrap_err(),
        "The accessibility sweep runs only on staging, and the staging url is production's."
    );
    s.production_url = Some("https://other.example".to_string());
    assert!(s.staging_origin(&PROBES_WORDS).is_ok());
    s.production_url = Some("garbage".to_string());
    assert!(s.staging_origin(&PROBES_WORDS).is_ok());
}

#[test]
fn pages_come_from_scan_then_lighthouse_as_paths() {
    let mut s = ScanSettings {
        lighthouse_pages: vec!["/l".into()],
        ..Default::default()
    };
    assert_eq!(s.listed_pages(), ["/l"]);
    s.pages = vec![" docs ".into(), "".into(), "/x".into()];
    assert_eq!(s.listed_pages(), ["/docs", "/x"]);
    assert!(ScanSettings::default().listed_pages().is_empty());
    assert!(is_retaskable("retaskable.com") && is_retaskable("a.retaskable.com"));
    assert!(!is_retaskable("notretaskable.com"));
}
