use super::*;

#[tokio::test]
async fn inside_the_agent_image_is_always_missing() {
    let s = probe("docker", true).await;
    assert_eq!(s.as_str(), "missing");
    assert!(s.reason().unwrap().contains("own Docker image"));
}

#[tokio::test]
async fn a_program_that_is_not_there_is_missing() {
    let s = probe("retaskable-no-such-docker", false).await;
    assert_eq!(s.as_str(), "missing");
    assert!(s.reason().unwrap().contains("isn't installed"));
}

#[cfg(unix)]
mod stub {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A `docker` that runs `body` for any arguments.
    fn stub(dir: &tempfile::TempDir, body: &str) -> std::path::PathBuf {
        let path = dir.path().join("docker");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[tokio::test]
    async fn answering_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let s = probe(stub(&dir, "echo Client; exit 0"), false).await;
        assert_eq!(s, DockerState::Ok);
        assert_eq!(s.reason(), None);
    }

    #[tokio::test]
    async fn a_daemon_error_is_stopped_with_the_reason() {
        let dir = tempfile::tempdir().unwrap();
        let p = stub(&dir, "echo 'Cannot connect to the Docker daemon' >&2; exit 1");
        let s = probe(p, false).await;
        assert_eq!(s.as_str(), "stopped");
        assert_eq!(s.reason(), Some("Docker isn't running: this computer won't run CI"));
    }

    #[tokio::test]
    async fn no_permission_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let p = stub(&dir, "echo 'permission denied while trying to connect' >&2; exit 1");
        let s = probe(p, false).await;
        assert_eq!(s.as_str(), "missing");
        assert!(s.reason().unwrap().contains("allowed"));
    }

    /// `probe_image`, run again while a parallel test's fork still holds the stub open for writing.
    async fn probe_image_settled(docker: &std::path::Path) -> DockerState {
        for _ in 0..50 {
            let s = probe_image(docker, "img").await;
            if !s.reason().is_some_and(|r| r.contains("Text file busy")) {
                return s;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        probe_image(docker, "img").await
    }

    #[tokio::test]
    async fn the_image_probe_runs_the_image_and_reports_a_failed_pull() {
        let dir = tempfile::tempdir().unwrap();
        let ok = stub(&dir, r#"[ "$1 $2 $3 $4" = "run --rm img true" ]"#);
        assert_eq!(probe_image_settled(&ok).await, DockerState::Ok);
        let bad = stub(&dir, "echo 'pull access denied' >&2; exit 1");
        let s = probe_image_settled(&bad).await;
        assert_eq!(s.as_str(), "stopped");
        assert!(s.reason().unwrap().contains("pull access denied"));
    }
}
