use super::fixture::{Fx, staging};
use crate::promote::{Promoter, Stop};

fn refused(why: &str) -> Result<(), Stop> {
    Err(Stop::Refused(why.into()))
}

fn site(fx: &Fx, perms: &[(&str, &str)], url: Option<&str>) -> Promoter {
    fx.promoter(fx.gh("m", Some((true, true)), perms), url)
}

#[tokio::test]
async fn staging_serving_the_commit_and_an_admin_issuer_pass() {
    let fx = Fx::new();
    let url = staging(200, &format!("{{\"sha\":\"{}\"}}", fx.c3.to_uppercase())).await;
    let p = site(&fx, &[("ann", "admin")], Some(&url));
    // The served sha starts with the requested one, whatever the case.
    assert_eq!(p.site_checks(&fx.c3[..7], "ann").await, Ok(()));
    assert_eq!(
        fx.gh_calls(),
        ["api repos/o/r/collaborators/ann/permission --jq .permission"]
    );
}

#[tokio::test]
async fn staging_serving_another_commit_is_refused_before_anyone_is_asked() {
    let fx = Fx::new();
    let url = staging(200, &format!("{{\"sha\":\"{}\"}}", fx.c2)).await;
    let p = site(&fx, &[("ann", "admin")], Some(&url));
    assert_eq!(
        p.site_checks(&fx.c3, "ann").await,
        refused(&format!(
            "Staging serves {}, not {}: check again.",
            &fx.c2[..7],
            &fx.c3[..7]
        ))
    );
    assert!(fx.gh_calls().is_empty());
    assert_eq!(
        p.from_site(&fx.c3, "ann").await,
        Err(Stop::Refused(format!(
            "Staging serves {}, not {}: check again.",
            &fx.c2[..7],
            &fx.c3[..7]
        )))
    );
    assert_eq!(fx.production(), fx.c1, "nothing is pushed");
}

#[tokio::test]
async fn staging_that_cant_say_what_it_serves_refuses_closed() {
    let fx = Fx::new();
    let sha = &fx.c3;
    let no_address = site(&fx, &[], None);
    assert_eq!(
        no_address.site_checks(sha, "ann").await,
        refused("Staging has no address to check: deploy it from the machine.")
    );
    for (status, body) in [
        (500, "{\"sha\":\"abcdef1\"}"),
        (200, "not json"),
        (200, "{}"),
        (200, "{\"sha\":\"abc\"}"),
        (200, "{\"sha\":\"zzzzzzzz\"}"),
        (200, "{\"sha\":7}"),
    ] {
        let url = staging(status, body).await;
        let why =
            format!("Staging doesn't say which commit it serves at {url}/version.json: deploy it from the machine.");
        assert_eq!(
            site(&fx, &[], Some(&url)).site_checks(sha, "ann").await,
            refused(&why),
            "{status} {body}"
        );
    }
    // Nothing is listening: refused, not hung.
    let gone = "http://127.0.0.1:1";
    assert!(matches!(
        site(&fx, &[], Some(gone)).site_checks(sha, "ann").await,
        Err(Stop::Refused(_))
    ));
}

#[tokio::test]
async fn the_version_path_setting_is_where_staging_is_asked() {
    let fx = Fx::new();
    let url = staging(200, &format!("{{\"sha\":\"{}\"}}", fx.c3)).await;
    let mut p = site(&fx, &[("ann", "admin")], Some(&format!("{url}/")));
    p.deploy.staging.as_mut().unwrap().version_path = Some("elsewhere.json".into());
    let Err(Stop::Refused(why)) = p.site_checks(&fx.c3, "ann").await else {
        panic!("only /version.json is served")
    };
    assert!(why.contains(&format!("{url}/elsewhere.json")), "{why}");
}

#[tokio::test]
async fn an_issuer_that_is_not_a_login_never_reaches_github() {
    let fx = Fx::new();
    let url = staging(200, &format!("{{\"sha\":\"{}\"}}", fx.c3)).await;
    let p = site(&fx, &[], Some(&url));
    for who in ["", "-x", "a/b", "../../user", "a b"] {
        assert_eq!(
            p.site_checks(&fx.c3, who).await,
            refused("Only the repo's admins can deploy from the website."),
            "{who:?}"
        );
    }
    assert!(fx.gh_calls().is_empty());
}

#[tokio::test]
async fn an_issuer_who_is_not_an_admin_is_refused_in_the_exact_words() {
    let fx = Fx::new();
    let url = staging(200, &format!("{{\"sha\":\"{}\"}}", fx.c3)).await;
    let p = site(
        &fx,
        &[("wendy", "write"), ("rae", "read"), ("nobody", "none"), ("blank", "")],
        Some(&url),
    );
    for (who, has) in [
        ("wendy", "write"),
        ("rae", "read"),
        ("nobody", "no access"),
        ("blank", "no access"),
    ] {
        assert_eq!(
            p.site_checks(&fx.c3, who).await,
            refused(&format!(
                "Only the repo's admins can deploy from the website (@{who} has {has})."
            )),
            "{who}"
        );
    }
    // gh couldn't say (this fake knows nobody called "stranger").
    assert_eq!(
        p.site_checks(&fx.c3, "stranger").await,
        refused(
            "Couldn't ask GitHub whether @stranger is an admin of o/r: gh: unexpected api repos/o/r/collaborators/stranger/permission --jq .permission"
        )
    );
    assert_eq!(
        p.from_site(&fx.c3, "wendy").await,
        Err(Stop::Refused(
            "Only the repo's admins can deploy from the website (@wendy has write).".into()
        ))
    );
    assert_eq!(fx.production(), fx.c1);
}

#[tokio::test]
async fn an_admin_from_the_website_pushes_and_the_row_names_them() {
    let fx = Fx::new();
    let url = staging(200, &format!("{{\"sha\":\"{}\"}}", fx.c3)).await;
    let p = site(&fx, &[("ann", "admin")], Some(&url));
    assert_eq!(p.from_site(&fx.c3[..10], "ann").await, Ok(fx.c3.clone()));
    assert_eq!(fx.production(), fx.c3);
    let rows: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(fx.pending_file()).unwrap()).unwrap();
    assert_eq!(rows[0]["entry"]["login"], "ann");
    assert_eq!(rows[0]["entry"]["via"], "site");
    // The machine's own admin check still ran, after the issuer's.
    assert_eq!(
        fx.gh_calls(),
        [
            "api repos/o/r/collaborators/ann/permission --jq .permission",
            "api repos/o/r --jq .permissions"
        ]
    );
}
