use serde_json::json;

use super::*;

fn ids(list: &[Audience]) -> Vec<&str> {
    list.iter().map(|a| a.id.as_str()).collect()
}

#[test]
fn no_book_means_the_starters_only() {
    for data in [json!(null), json!({}), json!("x")] {
        let list = audiences_of(&data);
        assert_eq!(list.len(), 8);
        assert_eq!(list[0].id, "admin");
        assert_eq!(list[7].id, "infrastructure");
    }
}

#[test]
fn added_audiences_follow_the_starters_and_a_repeated_id_is_dropped() {
    let data = json!({ "added": [
        { "id": "vendor", "name": "Vendor", "who": "Supplies us.", "part": "platform", "trial": false },
        { "id": "admin", "name": "Other admin", "who": "x", "part": "admin", "trial": false },
        { "id": "bad", "name": "  ", "who": "x", "part": "admin", "trial": false },
        { "nope": 1 } ] });
    let list = audiences_of(&data);
    assert_eq!(ids(&list)[8..], ["vendor"]);
    assert_eq!(list.len(), 9);
    assert_eq!(list[0].name, "Admin");
}

#[test]
fn edits_apply_and_a_blank_edit_falls_back() {
    let data = json!({ "edits": {
        "admin": { "name": " Boss ", "who": "Runs it all.", "part": "platform", "trial": true },
        "guest": { "name": "   ", "who": "" },
        "gone": { "name": "Nobody" } } });
    let list = audiences_of(&data);
    let admin = &list[0];
    assert_eq!(
        (
            admin.name.as_str(),
            admin.who.as_str(),
            admin.part.as_str(),
            admin.trial
        ),
        ("Boss", "Runs it all.", "platform", true)
    );
    let guest = list.iter().find(|a| a.id == "guest").unwrap();
    assert_eq!(
        (guest.name.as_str(), guest.who.as_str()),
        ("Guest", "Looks around the shop before signing in.")
    );
    assert_eq!(list.len(), 8);
}
