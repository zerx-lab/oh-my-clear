//! A tiny JSON reader for the few app files the junk scan inspects (Chromium `Local
//! State`, `package.json`). Only what those need: objects, arrays, strings; numbers and
//! literals are kept as raw text. Nesting depth is capped so hostile files cannot recurse
//! deeply.

/// Deepest nesting accepted.
const MAX_DEPTH: u32 = 128;

/// A parsed JSON value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Value {
    /// `null`, `true`, `false` or a number, as written.
    Raw(String),
    /// A string.
    Str(String),
    /// An array.
    Arr(Vec<Value>),
    /// An object, in file order.
    Obj(Vec<(String, Value)>),
}

impl Value {
    /// The member `key` of an object.
    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Self::Obj(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The members of an object (empty for other values).
    pub(crate) fn members(&self) -> &[(String, Value)] {
        match self {
            Self::Obj(members) => members,
            _ => &[],
        }
    }

    /// The text of a string value.
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// Parses a whole document; `None` when it is not valid JSON.
pub(crate) fn parse(text: &str) -> Option<Value> {
    let mut parser = Parser {
        chars: text.chars().peekable(),
    };
    let value = parser.value(0)?;
    parser.skip_ws();
    parser.chars.peek().is_none().then_some(value)
}

struct Parser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl Parser<'_> {
    fn skip_ws(&mut self) {
        while self.chars.next_if(|c| c.is_whitespace()).is_some() {}
    }

    fn value(&mut self, depth: u32) -> Option<Value> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.skip_ws();
        match *self.chars.peek()? {
            '{' => self.object(depth),
            '[' => self.array(depth),
            '"' => self.string().map(Value::Str),
            _ => self.raw(),
        }
    }

    fn object(&mut self, depth: u32) -> Option<Value> {
        self.chars.next();
        let mut members = Vec::new();
        self.skip_ws();
        if self.chars.next_if_eq(&'}').is_some() {
            return Some(Value::Obj(members));
        }
        loop {
            self.skip_ws();
            let key = self.string()?;
            self.skip_ws();
            self.chars.next_if_eq(&':')?;
            let value = self.value(depth.saturating_add(1))?;
            members.push((key, value));
            self.skip_ws();
            match self.chars.next()? {
                ',' => {}
                '}' => return Some(Value::Obj(members)),
                _ => return None,
            }
        }
    }

    fn array(&mut self, depth: u32) -> Option<Value> {
        self.chars.next();
        let mut items = Vec::new();
        self.skip_ws();
        if self.chars.next_if_eq(&']').is_some() {
            return Some(Value::Arr(items));
        }
        loop {
            items.push(self.value(depth.saturating_add(1))?);
            self.skip_ws();
            match self.chars.next()? {
                ',' => {}
                ']' => return Some(Value::Arr(items)),
                _ => return None,
            }
        }
    }

    fn raw(&mut self) -> Option<Value> {
        let mut out = String::new();
        while let Some(c) = self
            .chars
            .next_if(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '+' | '.'))
        {
            out.push(c);
        }
        (!out.is_empty()).then_some(Value::Raw(out))
    }

    fn string(&mut self) -> Option<String> {
        self.chars.next_if_eq(&'"')?;
        let mut out = String::new();
        loop {
            match self.chars.next()? {
                '"' => return Some(out),
                '\\' => match self.chars.next()? {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'u' => out.push(self.unicode_escape()?),
                    other => out.push(other),
                },
                c => out.push(c),
            }
        }
    }

    fn hex4(&mut self) -> Option<u32> {
        let mut n = 0_u32;
        for _ in 0..4 {
            let digit = self.chars.next()?.to_digit(16)?;
            n = n.checked_mul(16)?.checked_add(digit)?;
        }
        Some(n)
    }

    fn unicode_escape(&mut self) -> Option<char> {
        let first = self.hex4()?;
        if (0xD800..0xDC00).contains(&first) {
            // High surrogate: a `\uDC00`–`\uDFFF` low surrogate should follow.
            if self.chars.next_if_eq(&'\\').is_some() && self.chars.next_if_eq(&'u').is_some() {
                let low = self.hex4()?;
                let high = first.checked_sub(0xD800)?;
                let low = low.checked_sub(0xDC00)?;
                let code = high
                    .checked_mul(0x400)?
                    .checked_add(low)?
                    .checked_add(0x1_0000)?;
                return Some(char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER));
            }
            return Some(char::REPLACEMENT_CHARACTER);
        }
        Some(char::from_u32(first).unwrap_or(char::REPLACEMENT_CHARACTER))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_objects_and_escapes() {
        let doc = r#" {"profile": {"info_cache": {"Default": {"name": "Ma\u00efl \"x\"", "n": 3, "b": true},
            "Profile 1": {"name": "\ud83d\ude00"}}}, "list": [1, null, "a"]} "#;
        let value = parse(doc);
        let cache = value
            .as_ref()
            .and_then(|v| v.get("profile"))
            .and_then(|v| v.get("info_cache"));
        assert_eq!(
            cache.map(|c| c.members().len()),
            Some(2),
            "two profiles: {value:?}"
        );
        assert_eq!(
            cache
                .and_then(|c| c.get("Default"))
                .and_then(|p| p.get("name"))
                .and_then(Value::as_str),
            Some("Maïl \"x\""),
            "escapes decoded"
        );
        assert_eq!(
            cache
                .and_then(|c| c.get("Profile 1"))
                .and_then(|p| p.get("name"))
                .and_then(Value::as_str),
            Some("😀"),
            "surrogate pair decoded"
        );
    }

    #[test]
    fn rejects_malformed_and_too_deep_input() {
        assert_eq!(parse("{\"a\": }"), None, "missing value");
        assert_eq!(parse("{} x"), None, "trailing garbage");
        let deep = "[".repeat(1000);
        assert_eq!(parse(&deep), None, "depth is capped");
    }
}
