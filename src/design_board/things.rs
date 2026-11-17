//! A thing on a board, as the hub keeps it (TS `app/src/ui/whiteboardThings.ts`'s `BoardThing`, and the `things`
//! of `shared/boardDoc.ts`): whatever fields its kind has, plus its id. The drawing needs only a few kinds' boxes,
//! so a thing is its JSON object with typed reads, never a struct per kind, and a kind this agent doesn't know is
//! read and kept as it is.
use serde_json::{Map, Value, json};

/// A sticky's size (`STICKY`).
pub const STICKY: f64 = 200.0;
const STAMP: f64 = 56.0;
const PIN: f64 = 36.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Thing(pub Map<String, Value>);

/// A where-and-how-big box, `w` and `h` not negative.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// A number as JSON writes it where it is whole (`100`, not `100.0`), as the app's own things are.
pub fn num(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9e15 {
        json!(n as i64)
    } else {
        json!(n)
    }
}

/// `Math.round`: halves go up, also below zero.
pub fn js_round(n: f64) -> f64 {
    (n + 0.5).floor()
}

impl Thing {
    pub fn from_value(v: &Value) -> Option<Self> {
        let o = v.as_object()?;
        o.get("id")?.as_str()?;
        Some(Self(o.clone()))
    }

    pub fn id(&self) -> &str {
        self.str("id").unwrap_or_default()
    }

    pub fn kind(&self) -> &str {
        self.str("kind").unwrap_or_default()
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(Value::as_str)
    }

    pub fn f64(&self, key: &str) -> Option<f64> {
        self.0.get(key).and_then(Value::as_f64)
    }

    /// The mark of how the chat placed it, when it did (`chat`).
    pub fn chat(&self) -> Option<&str> {
        self.str("chat")
    }

    pub fn with_chat(mut self, mark: String) -> Self {
        self.0.insert("chat".into(), Value::String(mark));
        self
    }

    /// Where a thing is, as a box with a positive size (`boundsOf`). A pen line and a connector, which hang on
    /// their points and ends, are nothing here: nothing the drawing does reads them.
    pub fn bounds(&self) -> Bounds {
        let g = |k: &str| self.f64(k).unwrap_or(0.0);
        let sum = |k: &str| {
            self.0
                .get(k)
                .and_then(Value::as_array)
                .map_or(0.0, |a| a.iter().filter_map(Value::as_f64).sum())
        };
        let (x, y) = (g("x"), g("y"));
        match self.kind() {
            "stroke" | "connector" => Bounds {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            },
            "text" => {
                let size = match self.str("size") {
                    Some("s") => 16.0,
                    Some("l") => 36.0,
                    Some("xl") => 56.0,
                    _ => 24.0,
                };
                Bounds {
                    x,
                    y,
                    w: g("w"),
                    h: f64::max(40.0, size * 1.4),
                }
            }
            "stamp" => Bounds {
                x,
                y,
                w: STAMP,
                h: STAMP,
            },
            "comment" => Bounds {
                x,
                y: y - PIN,
                w: PIN,
                h: PIN,
            },
            "table" => Bounds {
                x,
                y,
                w: sum("cols"),
                h: sum("rows"),
            },
            _ => {
                let (w, h) = (g("w"), g("h"));
                Bounds {
                    x: x.min(x + w),
                    y: y.min(y + h),
                    w: w.abs(),
                    h: h.abs(),
                }
            }
        }
    }

    /// Whether this kind is drawn with lines through the board rather than a place on it: left out of what the
    /// session reads and of what the new design is kept clear of.
    pub fn is_line(&self) -> bool {
        matches!(self.kind(), "stroke" | "connector")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thing(v: Value) -> Thing {
        Thing::from_value(&v).unwrap()
    }

    #[test]
    fn a_things_box_by_kind() {
        let b = thing(json!({"id": "a", "kind": "sticky", "x": 10, "y": 20, "w": -30, "h": 40})).bounds();
        assert_eq!((b.x, b.y, b.w, b.h), (-20.0, 20.0, 30.0, 40.0));
        let t = thing(json!({"id": "t", "kind": "text", "x": 1, "y": 2, "w": 280, "size": "xl"})).bounds();
        assert_eq!((t.w, t.h), (280.0, 56.0 * 1.4));
        let c = thing(json!({"id": "c", "kind": "comment", "x": 5, "y": 100})).bounds();
        assert_eq!((c.y, c.h), (64.0, 36.0));
        let table =
            thing(json!({"id": "tb", "kind": "table", "x": 0, "y": 0, "cols": [120, 120], "rows": [52]})).bounds();
        assert_eq!((table.w, table.h), (240.0, 52.0));
        assert!(
            Thing::from_value(&json!({"kind": "sticky"})).is_none(),
            "a thing has an id"
        );
    }

    #[test]
    fn numbers_are_written_whole_and_rounded_as_the_app_does() {
        assert_eq!(num(100.0), json!(100));
        assert_eq!(num(191.75), json!(191.75));
        assert_eq!((js_round(2.5), js_round(-2.5), js_round(-0.4)), (3.0, -2.0, 0.0));
    }
}
