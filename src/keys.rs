use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Parsed user-facing keybind. Same string syntax we already accept in the
/// config: `ctrl-o`, `alt-g`, `f2`, `enter`, `?`, etc. — case-insensitive,
/// hyphen-separated modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyBind {
    pub mods: KeyModifiers,
    pub code: KeyCode,
}

impl KeyBind {
    pub fn parse(spec: &str) -> Option<KeyBind> {
        let lower = spec.trim().to_ascii_lowercase();
        if lower.is_empty() {
            return None;
        }
        let mut mods = KeyModifiers::NONE;
        let mut rest = lower.as_str();
        loop {
            if let Some(after) = rest.strip_prefix("ctrl-") {
                mods.insert(KeyModifiers::CONTROL);
                rest = after;
            } else if let Some(after) = rest.strip_prefix("alt-") {
                mods.insert(KeyModifiers::ALT);
                rest = after;
            } else if let Some(after) = rest.strip_prefix("shift-") {
                mods.insert(KeyModifiers::SHIFT);
                rest = after;
            } else {
                break;
            }
        }
        let code = match rest {
            "enter" | "return" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backspace" | "bs" => KeyCode::Backspace,
            "space" => KeyCode::Char(' '),
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" | "pgup" => KeyCode::PageUp,
            "pagedown" | "pgdn" | "pgdown" => KeyCode::PageDown,
            "insert" | "ins" => KeyCode::Insert,
            "delete" | "del" => KeyCode::Delete,
            other if other.starts_with('f') && other[1..].chars().all(|c| c.is_ascii_digit()) => {
                let n: u8 = other[1..].parse().ok()?;
                KeyCode::F(n)
            }
            other if other.chars().count() == 1 => {
                let ch = other.chars().next().unwrap();
                // Crossterm gives uppercase chars when SHIFT is held; for plain
                // single-char binds we lowercase to match the typical event.
                KeyCode::Char(ch)
            }
            _ => return None,
        };
        Some(KeyBind { mods, code })
    }

    /// Human-readable rendering (e.g. `Ctrl-U`, `Alt-G`, `F2`, `Enter`).
    /// Used by the help overlay and the keybind-conflict warning.
    pub fn display(self) -> String {
        let mut out = String::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            out.push_str("Ctrl-");
        }
        if self.mods.contains(KeyModifiers::ALT) {
            out.push_str("Alt-");
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            out.push_str("Shift-");
        }
        let key = match self.code {
            KeyCode::Char(' ') => "Space".to_string(),
            KeyCode::Char(c) => c.to_ascii_uppercase().to_string(),
            KeyCode::Enter => "Enter".to_string(),
            KeyCode::Esc => "Esc".to_string(),
            KeyCode::Tab => "Tab".to_string(),
            KeyCode::Backspace => "Backspace".to_string(),
            KeyCode::Up => "Up".to_string(),
            KeyCode::Down => "Down".to_string(),
            KeyCode::Left => "Left".to_string(),
            KeyCode::Right => "Right".to_string(),
            KeyCode::Home => "Home".to_string(),
            KeyCode::End => "End".to_string(),
            KeyCode::PageUp => "PageUp".to_string(),
            KeyCode::PageDown => "PageDown".to_string(),
            KeyCode::Insert => "Insert".to_string(),
            KeyCode::Delete => "Delete".to_string(),
            KeyCode::F(n) => format!("F{n}"),
            other => format!("{other:?}"),
        };
        out.push_str(&key);
        out
    }

    pub fn matches(self, event: KeyEvent) -> bool {
        // For printable characters, ignore SHIFT (the underlying char already
        // reflects it) so a bind on `?` matches whether or not SHIFT is in
        // the modifier set.
        let event_mods = if matches!(event.code, KeyCode::Char(_)) {
            event.modifiers - KeyModifiers::SHIFT
        } else {
            event.modifiers
        };
        let want_mods = if matches!(self.code, KeyCode::Char(_)) {
            self.mods - KeyModifiers::SHIFT
        } else {
            self.mods
        };
        if event_mods != want_mods {
            return false;
        }
        match (self.code, event.code) {
            (KeyCode::Char(a), KeyCode::Char(b)) => a.eq_ignore_ascii_case(&b),
            (a, b) => a == b,
        }
    }
}

/// A single built-in, **non-user-configurable** keybind: the key spec, how it
/// is shown on screen, and a one-line description. This is the single source
/// of truth for the fixed bindings — match sites call [`Builtin::matches`] and
/// the hint/help UI reads [`Builtin::label`]/[`Builtin::desc`], so changing a
/// built-in binding means editing exactly one entry in [`builtin`] below and
/// everything (matching *and* on-screen labels) follows.
///
/// Scope note: only bindings expressed as modifier chords or function keys
/// live here (F1/F2/Alt-*/Ctrl-*). Pure navigation/editing keys — arrows,
/// PageUp/Down, Home/End, Enter, Esc, Backspace, Delete — are matched directly
/// on their `KeyCode` variant at the call site; `KeyCode::Enter` is already a
/// single, unambiguous definition, so wrapping it here would only add a second
/// source of truth, not remove one.
#[derive(Debug, Clone, Copy)]
pub struct Builtin {
    /// Key spec in [`KeyBind::parse`] syntax (e.g. `"alt-g"`). Also the
    /// canonical thing to edit when rebinding.
    spec: &'static str,
    /// How the binding is presented to the user (e.g. `"Alt-G"`).
    pub label: &'static str,
    /// Short description for the help overlay.
    pub desc: &'static str,
}

impl Builtin {
    /// Parse the spec into a [`KeyBind`]. Specs are compile-time constants
    /// authored in this file, so a parse failure is a programmer error.
    pub fn bind(self) -> KeyBind {
        KeyBind::parse(self.spec).expect("built-in keybind spec must be valid")
    }

    pub fn matches(self, event: KeyEvent) -> bool {
        self.bind().matches(event)
    }
}

/// The fixed keybinds. Edit an entry here to rebind it everywhere.
pub mod builtin {
    use super::Builtin;

    /// Toggle the help overlay. `?` is deliberately NOT bound — it must reach
    /// the query (glob wildcard / regex `(?i)` flags).
    pub const HELP: Builtin = Builtin {
        spec: "f1",
        label: "F1",
        desc: "this help",
    };
    /// Toggle the settings overlay.
    pub const SETTINGS: Builtin = Builtin {
        spec: "f2",
        label: "F2",
        desc: "settings overlay",
    };
    /// Open the session search-mode picker (glob / regex).
    pub const MODE: Builtin = Builtin {
        spec: "f3",
        label: "F3",
        desc: "pick search mode (glob / regex)",
    };
    /// Open the session case-sensitivity picker.
    pub const CASE: Builtin = Builtin {
        spec: "f4",
        label: "F4",
        desc: "pick case sensitivity",
    };
    /// Open the session sort picker.
    pub const SORT: Builtin = Builtin {
        spec: "f5",
        label: "F5",
        desc: "pick sort (depth / alphabetical / blame_young / blame_old)",
    };
    /// Open the search-scope picker (working tree / branches).
    pub const SCOPE: Builtin = Builtin {
        spec: "f6",
        label: "F6",
        desc: "search scope (working tree / branches)",
    };
    /// Quit immediately, from anywhere.
    pub const QUIT: Builtin = Builtin {
        spec: "ctrl-c",
        label: "Ctrl-C",
        desc: "quit",
    };

    // Query editing (clear_query / delete_word / line_start / line_end) is NOT
    // here — those are ordinary `bind.<key> = <action>` entries (see
    // actions::Action), bound only if the user adds a line. Only truly fixed
    // keys live in `builtin`.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ke(mods: KeyModifiers, code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn parses_plain_letter() {
        let k = KeyBind::parse("o").unwrap();
        assert_eq!(k.mods, KeyModifiers::NONE);
        assert_eq!(k.code, KeyCode::Char('o'));
    }

    #[test]
    fn parses_ctrl_letter() {
        let k = KeyBind::parse("ctrl-o").unwrap();
        assert_eq!(k.mods, KeyModifiers::CONTROL);
        assert!(k.matches(ke(KeyModifiers::CONTROL, KeyCode::Char('o'))));
    }

    #[test]
    fn parses_alt_letter() {
        let k = KeyBind::parse("alt-g").unwrap();
        assert!(k.matches(ke(KeyModifiers::ALT, KeyCode::Char('g'))));
    }

    #[test]
    fn parses_fkeys() {
        let k = KeyBind::parse("f2").unwrap();
        assert_eq!(k.code, KeyCode::F(2));
        assert!(k.matches(ke(KeyModifiers::NONE, KeyCode::F(2))));
    }

    #[test]
    fn parses_named_keys() {
        assert_eq!(KeyBind::parse("enter").unwrap().code, KeyCode::Enter);
        assert_eq!(KeyBind::parse("esc").unwrap().code, KeyCode::Esc);
        assert_eq!(KeyBind::parse("pgup").unwrap().code, KeyCode::PageUp);
    }

    #[test]
    fn question_mark_matches_with_or_without_shift() {
        let k = KeyBind::parse("?").unwrap();
        assert!(k.matches(ke(KeyModifiers::NONE, KeyCode::Char('?'))));
        assert!(k.matches(ke(KeyModifiers::SHIFT, KeyCode::Char('?'))));
    }

    #[test]
    fn ctrl_letter_is_case_insensitive() {
        let k = KeyBind::parse("ctrl-O").unwrap();
        assert!(k.matches(ke(KeyModifiers::CONTROL, KeyCode::Char('o'))));
    }

    #[test]
    fn unknown_returns_none() {
        assert!(KeyBind::parse("nopekey").is_none());
        assert!(KeyBind::parse("").is_none());
    }

    #[test]
    fn every_builtin_spec_parses() {
        // `Builtin::bind` panics on a bad spec; assert each entry parses so a
        // typo surfaces here rather than at runtime in the key loop.
        use super::builtin::*;
        for b in [HELP, SETTINGS, MODE, CASE, SORT, QUIT] {
            assert!(
                KeyBind::parse(b.spec).is_some(),
                "built-in spec failed to parse: {} ({})",
                b.spec,
                b.label
            );
        }
    }
}
