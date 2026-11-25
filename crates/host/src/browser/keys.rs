//! Keys as `Input.dispatchKeyEvent` wants them. Chromium acts on a key's
//! Windows virtual key code (Enter submits, Backspace deletes), so a press
//! carries the web's `key`, its `code` and that number; text keys also carry
//! the text they type.

use agenthost_proto::BrowserMods;
use serde_json::{json, Value};

/// CDP's modifier bits.
pub fn mod_bits(m: &BrowserMods) -> u32 {
    (m.alt as u32) | (m.ctrl as u32) << 1 | (m.meta as u32) << 2 | (m.shift as u32) << 3
}

/// A named key: its code, virtual key code, and the text it types.
fn named(key: &str) -> Option<(&'static str, u32, Option<&'static str>)> {
    Some(match key {
        "Enter" => ("Enter", 13, Some("\r")),
        "Tab" => ("Tab", 9, None),
        "Backspace" => ("Backspace", 8, None),
        "Delete" => ("Delete", 46, None),
        "Escape" => ("Escape", 27, None),
        "ArrowLeft" => ("ArrowLeft", 37, None),
        "ArrowUp" => ("ArrowUp", 38, None),
        "ArrowRight" => ("ArrowRight", 39, None),
        "ArrowDown" => ("ArrowDown", 40, None),
        "Home" => ("Home", 36, None),
        "End" => ("End", 35, None),
        "PageUp" => ("PageUp", 33, None),
        "PageDown" => ("PageDown", 34, None),
        "Insert" => ("Insert", 45, None),
        " " | "Space" => ("Space", 32, Some(" ")),
        "F1" => ("F1", 112, None),
        "F2" => ("F2", 113, None),
        "F3" => ("F3", 114, None),
        "F4" => ("F4", 115, None),
        "F5" => ("F5", 116, None),
        "F6" => ("F6", 117, None),
        "F7" => ("F7", 118, None),
        "F8" => ("F8", 119, None),
        "F9" => ("F9", 120, None),
        "F10" => ("F10", 121, None),
        "F11" => ("F11", 122, None),
        "F12" => ("F12", 123, None),
        _ => return None,
    })
}

/// The down and up events for one press of `key` ("Enter", "a", "ArrowDown")
/// with `mods` held, typing `text` if given (else what the key types).
pub fn press(key: &str, text: Option<&str>, mods: &BrowserMods) -> Vec<Value> {
    let bits = mod_bits(mods);
    // Ctrl or Command with a key is a shortcut: it types nothing.
    let chord = mods.ctrl || mods.meta;
    let key = if key == "Space" { " " } else { key };
    let (code, vk, typed) = match named(key) {
        Some((code, vk, t)) => (code.to_string(), vk, t.map(str::to_string)),
        None => {
            let mut chars = key.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                // A key we have no name for: type its text, if any.
                return text.map(|t| vec![json!({"type": "keyDown", "key": key, "text": t, "unmodifiedText": t, "modifiers": bits}), json!({"type": "keyUp", "key": key, "modifiers": bits})]).unwrap_or_default();
            };
            let (code, vk) = if c.is_ascii_alphabetic() {
                (format!("Key{}", c.to_ascii_uppercase()), c.to_ascii_uppercase() as u32)
            } else if c.is_ascii_digit() {
                (format!("Digit{c}"), c as u32)
            } else {
                (String::new(), 0)
            };
            (code, vk, Some(c.to_string()))
        }
    };
    let typed = if chord { None } else { text.map(str::to_string).or(typed) };
    let mut down = json!({"key": key, "code": code, "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk, "modifiers": bits});
    match &typed {
        Some(t) => {
            down["type"] = json!("keyDown");
            down["text"] = json!(t);
            down["unmodifiedText"] = json!(t);
        }
        None => down["type"] = json!("rawKeyDown"),
    }
    // macOS handles editing shortcuts above the page; ask for them by name.
    if cfg!(target_os = "macos") && mods.meta {
        let cmd = match (key.to_ascii_lowercase().as_str(), mods.shift) {
            ("a", false) => Some("selectAll"),
            ("z", false) => Some("undo"),
            ("z", true) => Some("redo"),
            _ => None,
        };
        if let Some(c) = cmd {
            down["commands"] = json!([c]);
        }
    }
    let up = json!({"type": "keyUp", "key": key, "code": code, "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk, "modifiers": bits});
    vec![down, up]
}

/// An agent's key: "Enter", "Control+a", "Shift+Tab", "Meta+ArrowLeft".
pub fn parse_combo(s: &str) -> Result<(String, BrowserMods), String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("no key".into());
    }
    // "+" on its own (or at the end, as in "Shift++") is the key itself.
    let (mods_part, key) = match s.strip_suffix("++") {
        Some(rest) => (rest, "+"),
        None => match s.rsplit_once('+') {
            Some((m, k)) if !k.is_empty() => (m, k),
            _ => ("", s),
        },
    };
    let mut mods = BrowserMods::default();
    for m in mods_part.split('+').filter(|m| !m.is_empty()) {
        match m.to_ascii_lowercase().as_str() {
            "shift" => mods.shift = true,
            "control" | "ctrl" => mods.ctrl = true,
            "alt" | "option" => mods.alt = true,
            "meta" | "cmd" | "command" | "super" => mods.meta = true,
            other => return Err(format!("unknown modifier {other:?} (use Shift, Control, Alt or Meta)")),
        }
    }
    let key = match key.to_ascii_lowercase().as_str() {
        "enter" | "return" => "Enter",
        "esc" | "escape" => "Escape",
        "tab" => "Tab",
        "backspace" => "Backspace",
        "delete" | "del" => "Delete",
        "space" => " ",
        "left" | "arrowleft" => "ArrowLeft",
        "right" | "arrowright" => "ArrowRight",
        "up" | "arrowup" => "ArrowUp",
        "down" | "arrowdown" => "ArrowDown",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        _ => key,
    }
    .to_string();
    if key.chars().count() > 1 && named(&key).is_none() {
        return Err(format!("unknown key {key:?}"));
    }
    Ok((key, mods))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_types_a_return_and_carries_its_code() {
        let ev = press("Enter", None, &BrowserMods::default());
        assert_eq!(ev[0]["type"], "keyDown");
        assert_eq!(ev[0]["windowsVirtualKeyCode"], 13);
        assert_eq!(ev[0]["text"], "\r");
        assert_eq!(ev[1]["type"], "keyUp");
    }

    #[test]
    fn arrows_type_nothing() {
        let ev = press("ArrowDown", None, &BrowserMods::default());
        assert_eq!(ev[0]["type"], "rawKeyDown");
        assert!(ev[0].get("text").is_none());
    }

    #[test]
    fn letters_type_themselves_unless_a_shortcut() {
        let a = press("a", None, &BrowserMods::default());
        assert_eq!((a[0]["code"].as_str(), a[0]["text"].as_str(), a[0]["windowsVirtualKeyCode"].as_u64()), (Some("KeyA"), Some("a"), Some(65)));
        let ctrl = BrowserMods { ctrl: true, ..Default::default() };
        let c = press("a", None, &ctrl);
        assert_eq!(c[0]["type"], "rawKeyDown");
        assert_eq!(c[0]["modifiers"], 2);
        let shifted = press("A", Some("A"), &BrowserMods { shift: true, ..Default::default() });
        assert_eq!((shifted[0]["text"].as_str(), shifted[0]["modifiers"].as_u64()), (Some("A"), Some(8)));
    }

    #[test]
    fn combos_parse() {
        assert_eq!(parse_combo("Enter").unwrap().0, "Enter");
        let (k, m) = parse_combo("Control+Shift+a").unwrap();
        assert_eq!((k.as_str(), m.ctrl, m.shift, m.alt), ("a", true, true, false));
        assert_eq!(parse_combo("Shift++").unwrap().0, "+");
        assert_eq!(parse_combo("+").unwrap().0, "+");
        assert_eq!(parse_combo("esc").unwrap().0, "Escape");
        assert!(parse_combo("Hyper+a").is_err());
        assert!(parse_combo("NotAKey").is_err());
    }
}
