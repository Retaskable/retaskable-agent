use super::*;
use crate::env::Env;
use crate::settings_server::test_keychain;

fn server(port: u16) -> SettingsServer {
    let dir = tempfile::tempdir().unwrap();
    SettingsServer::new(
        dir.path().to_path_buf(),
        port,
        port,
        false,
        Env::empty(),
        || 0,
        |_: i32| {},
        test_keychain(dir.path()),
    )
}

#[test]
fn split_host_port_splits_a_plain_host_and_port() {
    assert_eq!(split_host_port("127.0.0.1:4700"), ("127.0.0.1", Some(4700)));
    assert_eq!(split_host_port("127.0.0.1"), ("127.0.0.1", None));
}

#[test]
fn split_host_port_keeps_an_ipv6_literals_brackets_as_part_of_the_host() {
    assert_eq!(split_host_port("[::1]:4700"), ("[::1]", Some(4700)));
    assert_eq!(split_host_port("[::1]"), ("[::1]", None));
}

#[test]
fn split_host_port_falls_back_to_the_whole_header_when_it_cant_make_sense_of_it() {
    // No closing bracket.
    assert_eq!(split_host_port("[::1"), ("[::1", None));
    // A trailing colon with no port.
    assert_eq!(split_host_port("host:"), ("host:", None));
    // A "port" that isn't numeric.
    assert_eq!(split_host_port("host:abc"), ("host:abc", None));
}

#[test]
fn host_is_allowed_only_for_loopback_names_at_the_agents_own_port() {
    let settings = server(4700);
    assert!(settings.host_is_allowed("127.0.0.1:4700"));
    assert!(settings.host_is_allowed("localhost:4700"));
    assert!(settings.host_is_allowed("[::1]:4700"));
    // Case doesn't matter.
    assert!(settings.host_is_allowed("LOCALHOST:4700"));
    // No port at all skips the port check.
    assert!(settings.host_is_allowed("127.0.0.1"));

    assert!(!settings.host_is_allowed("127.0.0.1:9999"), "wrong port");
    assert!(!settings.host_is_allowed("evil.example.com:4700"), "not loopback");
}

#[test]
fn parse_cookie_finds_the_named_cookie_among_several() {
    assert_eq!(
        parse_cookie(
            "a=1; retaskable_settings_session=abc123; b=2",
            "retaskable_settings_session"
        ),
        Some("abc123".to_string())
    );
}

#[test]
fn parse_cookie_is_none_when_the_name_isnt_there() {
    assert_eq!(parse_cookie("a=1; b=2", "retaskable_settings_session"), None);
}

#[test]
fn parse_cookie_only_splits_on_the_first_equals() {
    assert_eq!(parse_cookie("session=ab=cd", "session"), Some("ab=cd".to_string()));
}

#[test]
fn parse_cookie_matches_the_name_exactly_not_as_a_prefix() {
    assert_eq!(parse_cookie("sessions=1; session=2", "session"), Some("2".to_string()));
}

#[test]
fn constant_time_eq_compares_equal_and_unequal_strings() {
    assert!(constant_time_eq("same-token", "same-token"));
    assert!(constant_time_eq("", ""));
    assert!(!constant_time_eq("short", "longer-string"));
    assert!(!constant_time_eq("token-a", "token-b"));
}

#[test]
fn csrf_ok_checks_the_header_against_the_expected_token() {
    let mut headers = HeaderMap::new();
    headers.insert("x-csrf-token", "expected".parse().unwrap());
    assert!(csrf_ok(&headers, "expected"));
    assert!(!csrf_ok(&headers, "something-else"));
    assert!(!csrf_ok(&HeaderMap::new(), "expected"), "missing header");
}
