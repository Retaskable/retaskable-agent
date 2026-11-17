use super::*;
use serde_json::json;

fn run_piece(id: &str, item_title: Option<&str>, title: &str) -> RunPiece {
    let mut piece = json!({ "id": id, "title": title });
    if let Some(t) = item_title {
        piece["itemTitle"] = json!(t);
    }
    serde_json::from_value(piece).unwrap()
}

#[test]
fn a_piece_branch_is_its_record_id_and_the_slug_of_item_and_piece_titles() {
    assert_eq!(
        piece_branch_name(&run_piece("p7", None, "Build the implementer's prompt!")),
        "retaskable/p7-build-the-implementer-s-prompt-s0"
    );
    assert_eq!(
        piece_branch_name(&run_piece("i1-piece-2", Some("Intent rows"), "Take rows")),
        "retaskable/i1-piece-2-intent-rows-take-rows-s0"
    );
}

#[test]
fn a_piece_branch_passes_git_check_ref_format_whatever_the_store_sent() {
    let hostile = [
        "Intent rows: take draft_wanted rows for an accepted idea",
        "a~b^c:d?e*f[g\\h",
        "dots..and@{braces}",
        "ends in .lock",
        "x.lock",
        "-leading dash",
        "tr\u{e9}s \u{65e5}\u{672c}\u{8a9e} \u{1f680} title",
        "  ",
        "",
        "///",
        "a//b/./c",
        "@",
    ];
    for id in hostile.iter().chain(["i1-piece-3"].iter()) {
        for title in hostile {
            for item_title in [None, Some(title)] {
                let name = piece_branch_name(&run_piece(id, item_title, title));
                let ok = std::process::Command::new("git")
                    .args(["check-ref-format", "--branch", &name])
                    .output()
                    .unwrap();
                assert!(ok.status.success(), "{name:?} from id {id:?}, title {title:?}");
                assert!(name.starts_with("retaskable/") && name.ends_with("-s0"), "{name}");
                assert!(name.is_ascii() && !name.contains(".lock"), "{name}");
            }
        }
    }
}
