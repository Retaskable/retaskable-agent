use std::fs;
use std::path::Path;

use super::*;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (rel, text) in files {
        write(dir.path(), rel, text);
    }
    dir
}

#[test]
fn names_and_ids_read_like_a_person_would() {
    assert_eq!(name_of("UserProfileScreen"), "User profile");
    assert_eq!(name_of("user-profile"), "User profile");
    assert_eq!(name_of("Button.stories"), "Button");
    let mut taken = HashSet::new();
    let pat = re(r"^[A-Za-z][A-Za-z0-9-]{0,40}$");
    assert_eq!(id_of("Data grid", &mut taken, &pat, false), "dataGrid");
    assert_eq!(id_of("Data grid", &mut taken, &pat, false), "dataGrid2");
    assert_eq!(id_of("3d view", &mut taken, &pat, false), "p3dView");
    assert_eq!(
        id_of("Home", &mut HashSet::new(), &re(r"^[a-z][a-z0-9]{0,30}$"), true),
        "home"
    );
}

#[test]
fn a_kind_comes_from_the_name_and_an_unknown_one_is_a_card() {
    assert_eq!(kind_of("Primary button"), "button");
    assert_eq!(kind_of("Data grid"), "table");
    assert_eq!(kind_of("Bottom bar"), "tabBar");
    assert_eq!(kind_of("Frobnicator"), "card");
}

#[test]
fn a_storybook_with_stories_is_a_catalogue_with_their_options() {
    let dir = project(&[
        (
            "package.json",
            r#"{"dependencies":{"react":"^18.0.0"},"devDependencies":{"@storybook/react":"7"}}"#,
        ),
        (".storybook/main.ts", "export default {}"),
        (
            "src/Button.stories.tsx",
            "export default { title: 'Forms/Button', argTypes: {\n  size: { control: 'radio' },\n  disabled: {},\n} };",
        ),
    ]);
    let found = scan(dir.path());
    assert!(found.signals.contains(&"storybook".to_string()) && found.signals.contains(&"react".to_string()));
    assert_eq!(found.pieces.len(), 1);
    assert_eq!(found.pieces[0].name, "Button");
    assert_eq!(
        found.pieces[0].options.as_deref(),
        Some(&["size".to_string(), "disabled".to_string()][..])
    );
    let kit = choose(&found);
    assert_eq!((kit.tier, kit.genre), (Tier::Catalogue, Some(Genre::App)));
    assert!(read_kit(&serde_json::to_value(&kit).unwrap()).is_some());
}

#[test]
fn three_component_files_make_a_catalogue_and_two_do_not() {
    let comps = |n: usize| {
        let files: Vec<(String, String)> = ["Card", "Avatar", "Badge"][..n]
            .iter()
            .map(|c| (format!("src/components/{c}.tsx"), "export const x = 1;".to_string()))
            .collect();
        let dir = tempfile::tempdir().unwrap();
        for (rel, text) in &files {
            write(dir.path(), rel, text);
        }
        dir
    };
    assert_eq!(choose(&scan(comps(3).path())).tier, Tier::Catalogue);
    assert_eq!(choose(&scan(comps(2).path())).tier, Tier::Wireframe);
}

#[test]
fn a_lower_case_library_is_named_by_what_it_exports() {
    let dir = project(&[(
        "src/ui/buttons.tsx",
        "export function PrimaryButton() {}\nexport const GhostButton = 1;\nexport type ButtonProps = {};\nexport const ButtonOptions = 1;\nexport function Spinner() {}",
    )]);
    let names: Vec<String> = scan(dir.path()).pieces.into_iter().map(|p| p.name).collect();
    assert_eq!(names, ["Primary button", "Ghost button", "Spinner"]);
}

#[test]
fn a_game_a_grid_and_an_empty_folder_are_wireframes_of_their_genre() {
    let game = project(&[
        ("project.godot", ""),
        ("scenes/Main Menu.tscn", ""),
        ("scenes/probe_shots.tscn", ""),
    ]);
    let found = scan(game.path());
    assert_eq!(found.genre, Genre::Game);
    assert_eq!(
        found.pages.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        ["Main menu"]
    );
    let kit = choose(&found);
    assert_eq!((kit.tier, kit.pieces.len()), (Tier::Wireframe, 19));
    assert_eq!(kit.pages.unwrap()[0].kind, "scene");
    let grid = project(&[("package.json", r#"{"dependencies":{"ag-grid-community":"1"}}"#)]);
    assert_eq!(scan(grid.path()).genre, Genre::Data);
    let empty = tempfile::tempdir().unwrap();
    let kit = choose(&scan(empty.path()));
    assert_eq!((kit.tier, kit.genre), (Tier::Wireframe, Some(Genre::App)));
}

#[test]
fn retaskables_own_project_is_the_library_and_stores_nothing() {
    let dir = project(&[("app/src/ui/whiteboardDesign.ts", ""), ("shared/designLayout.ts", "")]);
    let found = scan(dir.path());
    let kit = choose(&found);
    assert_eq!((found.library, kit.tier, kit.pieces.len()), (true, Tier::Library, 0));
}

#[test]
fn tokens_come_from_theme_files_by_plain_patterns() {
    let theme = "export const colors = {\n  primary: '#123456',\n  'on-surface': '#fff',\n  name: 'x',\n};\n--brand-blue: #abcdef;\nconst spacing = [4, 8, 16, 1000];\nborderRadius: 8px, 12px;\nfont-family: 'Inter', sans-serif;\n";
    let t = tokens_from(&[theme.to_string()]);
    let colors = t.colors.unwrap();
    assert_eq!(colors["primary"], "#123456");
    assert_eq!(colors["onSurface"], "#ffffff");
    assert_eq!(colors["brandBlue"], "#abcdef");
    assert_eq!(t.spacing.unwrap(), [4.0, 8.0, 16.0]);
    assert_eq!(t.radius.unwrap(), [8.0, 12.0]);
    assert_eq!(t.fonts.unwrap(), ["Inter"]);
}

#[test]
fn pages_are_named_from_screen_folders_routes_and_top_level_html_only() {
    let dir = project(&[
        ("src/screens/UserProfileScreen.tsx", ""),
        ("src/screens/index.tsx", ""),
        ("src/screens/Home.test.tsx", ""),
        ("src/screens/outboxText.tsx", ""),
        ("app/settings/page.tsx", ""),
        ("app/layout.tsx", ""),
        ("pricing.html", ""),
        ("dist/old.html", ""),
        ("package.json", r#"{"dependencies":{"next":"14"}}"#),
    ]);
    let pages = scan(dir.path()).pages;
    let named: Vec<(&str, &str)> = pages.iter().map(|p| (p.name.as_str(), p.kind.as_str())).collect();
    assert!(named.contains(&("User profile", "screen")), "{named:?}");
    assert!(named.contains(&("Home", "screen")), "{named:?}");
    assert!(named.contains(&("Settings", "route")), "{named:?}");
    assert!(named.contains(&("Pricing", "screen")), "{named:?}");
    assert!(
        !named
            .iter()
            .any(|(n, _)| n.contains("test") || n.contains("Outbox") || n.contains("Old") || n.contains("Layout")),
        "{named:?}"
    );
}

#[test]
fn the_fingerprint_ignores_a_version_bump_and_moves_with_a_new_piece_and_nothing_is_run() {
    let a = project(&[
        ("package.json", r#"{"dependencies":{"react":"17"}}"#),
        ("src/components/Card.tsx", ""),
    ]);
    let b = project(&[
        ("package.json", r#"{"dependencies":{"react":"18"}}"#),
        ("src/components/Card.tsx", ""),
    ]);
    let c = project(&[
        ("package.json", r#"{"dependencies":{"react":"18"}}"#),
        ("src/components/Card.tsx", ""),
        ("src/components/Avatar.tsx", ""),
    ]);
    assert_eq!(scan(a.path()).fingerprint, scan(b.path()).fingerprint);
    assert_ne!(scan(b.path()).fingerprint, scan(c.path()).fingerprint);
    assert_eq!(scan(a.path()).fingerprint.len(), 16);
    // A script that would leave a mark if anything ran it.
    let marked = project(&[
        ("package.json", r#"{"scripts":{"postinstall":"touch RAN"}}"#),
        ("scripts/run.sh", "touch RAN"),
    ]);
    scan(marked.path());
    assert!(!marked.path().join("RAN").exists());
}

fn found_catalogue() -> Found {
    let dir = project(&[
        (".storybook/main.ts", ""),
        ("src/Button.stories.tsx", "title: 'Button'"),
        ("src/Card.stories.tsx", "title: 'Card'"),
    ]);
    scan(dir.path())
}

#[tokio::test]
async fn a_sessions_valid_tidy_is_the_kit_and_an_invalid_or_missing_one_falls_back_to_the_wireframe() {
    let found = found_catalogue();
    let good = json!({ "pieces": [{ "id": "primaryButton", "name": "Primary button", "kind": "button" }] });
    let (kit, how) = discover(
        &found,
        Some(|d: String| async move {
            assert!(d.contains("Pieces found:") && d.contains("- Button"));
            Some(good)
        }),
    )
    .await;
    assert_eq!((how, kit.pieces[0].id.as_str()), (How::Session, "primaryButton"));
    let bad = json!({ "pieces": [{ "id": "x", "name": "X", "kind": "hologram" }] });
    let (kit, how) = discover(&found, Some(|_: String| async move { Some(bad) })).await;
    assert_eq!((how, kit.tier), (How::Fallback, Tier::Wireframe));
    assert_eq!(kit.fingerprint.as_deref(), Some(found.fingerprint.as_str()));
    let (_, how) = discover(&found, Some(|_: String| async { None })).await;
    assert_eq!(how, How::Fallback);
    let (kit, how) = discover(&found, None::<fn(String) -> std::future::Ready<Option<Value>>>).await;
    assert_eq!((how, kit.tier), (How::Code, Tier::Catalogue));
}
