//! docs/specs/setup-wizard.md sections 2.1 and 2.2: the one install. The Docker socket the runner's entrypoint finds (the
//! script run over a directory of made-up sockets), and the compose file rendering with no environment variable set.
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[cfg(target_os = "linux")]
fn detect(host_run: &Path) -> String {
    use std::os::unix::process::CommandExt;
    let out = Command::new("sh")
        .arg(root().join("docker/ci/ci-start.sh"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOST_RUN", host_run)
        .env("DOCKER_SOCKET_DRY_RUN", "1")
        .arg0("ci-start.sh")
        .output()
        .expect("sh runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[cfg(target_os = "linux")]
fn socket(dir: &Path, name: &str) -> (PathBuf, u32) {
    use std::os::unix::fs::MetadataExt;
    let path = dir.join(name);
    // Kept open so the socket file stays: it is a real socket, which is what the script looks for (`-S`).
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    std::mem::forget(listener);
    (path.clone(), std::fs::metadata(&path).unwrap().gid())
}

#[cfg(target_os = "linux")]
#[test]
fn docker_desktops_raw_socket_wins_over_the_proxy_one() {
    let dir = tempfile::tempdir().unwrap();
    let (raw, gid) = socket(dir.path(), "docker.sock.raw");
    socket(dir.path(), "docker.sock");
    assert_eq!(detect(dir.path()), format!("socket={} gid={gid}", raw.display()));
}

#[cfg(target_os = "linux")]
#[test]
fn a_linux_server_has_only_docker_sock_and_its_group_is_the_sockets_own() {
    let dir = tempfile::tempdir().unwrap();
    let (sock, gid) = socket(dir.path(), "docker.sock");
    assert_eq!(detect(dir.path()), format!("socket={} gid={gid}", sock.display()));
}

#[cfg(target_os = "linux")]
#[test]
fn a_file_or_directory_that_is_not_a_socket_is_not_taken_and_none_says_none() {
    let dir = tempfile::tempdir().unwrap();
    // What a bind mount of a missing file leaves behind: a directory where the socket should be.
    std::fs::create_dir(dir.path().join("docker.sock.raw")).unwrap();
    std::fs::write(dir.path().join("docker.sock"), b"not a socket").unwrap();
    assert_eq!(detect(dir.path()), "socket=none gid=none");
    let (sock, _) = socket(dir.path(), "docker.sock.raw-other");
    assert!(sock.exists());
    assert_eq!(detect(&dir.path().join("missing")), "socket=none gid=none");
}

#[cfg(target_os = "linux")]
#[test]
fn run_as_a_plain_user_with_no_dry_run_it_only_runs_the_command() {
    use std::os::unix::process::CommandExt;
    // Not root, so the script's non-root branch: no socket search matters, the command runs as it is.
    if Command::new("id")
        .arg("-u")
        .output()
        .map(|o| o.stdout == b"0\n")
        .unwrap_or(false)
    {
        return;
    }
    let out = Command::new("sh")
        .arg(root().join("docker/ci/ci-start.sh"))
        .arg("echo")
        .arg("ran")
        .env("HOST_RUN", "/no/such/dir")
        .arg0("ci-start.sh")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ran");
}

/// `docker compose config` on `file` with nothing in the environment but PATH and HOME; `None` when Docker isn't here.
fn render(file: &Path) -> Option<String> {
    let probe = Command::new("docker").args(["compose", "version"]).output().ok()?;
    if !probe.status.success() {
        return None;
    }
    let out = Command::new("docker")
        .args(["compose", "-f"])
        .arg(file)
        .arg("config")
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .output()
        .ok()?;
    assert!(
        out.status.success(),
        "{} must render with no environment variable set: {}",
        file.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8(out.stdout).unwrap())
}

#[test]
fn the_one_compose_file_renders_with_no_environment_variable_and_has_both_programs() {
    let Some(text) = render(&root().join("docker/compose.yaml")) else {
        eprintln!("skipped: no docker compose here");
        return;
    };
    for needed in [
        "retaskable-agent:latest",
        "retaskable-ci:latest",
        "name: retaskable-data",
        "RETASKABLE_PORT: \"4710\"",
        "host_ip: 127.0.0.1",
        "RETASKABLE_CI_FROM_AGENT: \"1\"",
        "/host-run",
    ] {
        assert!(text.contains(needed), "missing {needed:?} in:\n{text}");
    }
    for never in ["DOCKER_GID", "AGENT_CI_NAME", "GIT_USER_NAME", "GH_TOKEN", "0.0.0.0"] {
        assert!(!text.contains(never), "{never:?} must not be needed:\n{text}");
    }
}

#[test]
fn the_agent_only_file_still_renders_with_no_environment_variable() {
    if let Some(text) = render(&root().join("docker/agent/compose.yaml")) {
        assert!(!text.contains("GIT_USER"));
    }
}

#[test]
fn the_server_only_ci_file_still_asks_for_what_it_needs() {
    // It is for a server: it names its socket group and computer by variable, and says so when they're missing.
    let file = root().join("docker/ci/compose.yaml");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("DOCKER_GID:?") && text.contains("AGENT_CI_NAME:?"));
}
