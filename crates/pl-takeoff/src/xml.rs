//! A small XML reader and writer: enough for XFDF and nothing more.
//!
//! The reader builds a tree of elements, attributes and text. It understands the XML
//! declaration and processing instructions (skipped), comments (skipped), a `DOCTYPE` (skipped;
//! nothing in it is processed, so no entity can be defined or expanded), CDATA sections, the
//! five predefined entities and numeric character references. Namespaces are not resolved:
//! names keep their prefix. Malformed input is an error with the byte offset, never a panic.

/// A syntax error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("XML error at byte {offset}: {message}")]
pub struct XmlError {
    pub offset: usize,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Element(Element),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Element {
    /// The name as written, prefix included.
    pub name: String,
    pub attributes: Vec<(String, String)>,
    pub children: Vec<Node>,
}

/// Elements nested deeper than this are refused.
const MAX_DEPTH: usize = 200;

/// The most attributes one element may have. Nothing XFDF defines comes near it.
pub const MAX_ATTRIBUTES: usize = 4096;

impl Element {
    pub fn new(name: &str) -> Element {
        Element {
            name: name.to_string(),
            ..Element::default()
        }
    }

    /// The name without its namespace prefix.
    pub fn local_name(&self) -> &str {
        self.name.rsplit(':').next().unwrap_or(&self.name)
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Sets an attribute (builder style).
    pub fn with(mut self, name: &str, value: impl Into<String>) -> Element {
        self.set(name, value);
        self
    }

    pub fn set(&mut self, name: &str, value: impl Into<String>) {
        let value = value.into();
        match self.attributes.iter_mut().find(|(k, _)| k == name) {
            Some(slot) => slot.1 = value,
            None => self.attributes.push((name.to_string(), value)),
        }
    }

    pub fn push(&mut self, child: Element) {
        self.children.push(Node::Element(child));
    }

    pub fn push_text(&mut self, text: &str) {
        self.children.push(Node::Text(text.to_string()));
    }

    /// The child elements.
    pub fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|n| match n {
            Node::Element(e) => Some(e),
            Node::Text(_) => None,
        })
    }

    /// The first child element with this exact name.
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.elements().find(|e| e.name == name)
    }

    /// The first element with this exact name anywhere below, depth first.
    pub fn descendant(&self, name: &str) -> Option<&Element> {
        for e in self.elements() {
            if e.name == name {
                return Some(e);
            }
            if let Some(found) = e.descendant(name) {
                return Some(found);
            }
        }
        None
    }

    /// The text directly inside the element.
    pub fn text(&self) -> String {
        self.children
            .iter()
            .filter_map(|n| match n {
                Node::Text(t) => Some(t.as_str()),
                Node::Element(_) => None,
            })
            .collect()
    }

    /// Serialises the element. Text and attribute values are escaped so that the reader gives
    /// back exactly the same strings (line breaks and tabs included); characters XML cannot
    /// hold at all (control characters other than tab, line feed and carriage return) are
    /// left out.
    pub fn write(&self, out: &mut String) {
        out.push('<');
        out.push_str(&self.name);
        for (k, v) in &self.attributes {
            out.push(' ');
            out.push_str(k);
            out.push_str("=\"");
            escape(v, true, out);
            out.push('"');
        }
        if self.children.is_empty() {
            out.push_str("/>");
            return;
        }
        out.push('>');
        for child in &self.children {
            match child {
                Node::Element(e) => e.write(out),
                Node::Text(t) => escape(t, false, out),
            }
        }
        out.push_str("</");
        out.push_str(&self.name);
        out.push('>');
    }
}

fn allowed(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..)
}

fn escape(s: &str, attribute: bool, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            // A reader turns a literal CR into LF, and line breaks in an attribute into spaces.
            '\r' => out.push_str("&#13;"),
            '\n' if attribute => out.push_str("&#10;"),
            '\t' if attribute => out.push_str("&#9;"),
            c if allowed(c) => out.push(c),
            _ => {}
        }
    }
}

struct Parser<'a> {
    src: &'a str,
    at: usize,
}

impl<'a> Parser<'a> {
    fn err<T>(&self, message: &str) -> Result<T, XmlError> {
        Err(XmlError {
            offset: self.at,
            message: message.to_string(),
        })
    }

    fn rest(&self) -> &'a str {
        &self.src[self.at..]
    }

    fn eat(&mut self, token: &str) -> bool {
        if self.rest().starts_with(token) {
            self.at += token.len();
            true
        } else {
            false
        }
    }

    fn skip_space(&mut self) {
        let trimmed = self.rest().trim_start_matches([' ', '\t', '\r', '\n']);
        self.at = self.src.len() - trimmed.len();
    }

    /// Skips to just after `end`.
    fn skip_past(&mut self, end: &str, what: &str) -> Result<(), XmlError> {
        match self.rest().find(end) {
            Some(i) => {
                self.at += i + end.len();
                Ok(())
            }
            None => self.err(&format!("{what} is not closed")),
        }
    }

    /// Comments, processing instructions, a DOCTYPE and white space between elements.
    fn skip_misc(&mut self, allow_doctype: bool) -> Result<(), XmlError> {
        loop {
            self.skip_space();
            if self.eat("<!--") {
                self.skip_past("-->", "a comment")?;
            } else if self.eat("<?") {
                self.skip_past("?>", "a processing instruction")?;
            } else if allow_doctype && self.rest().starts_with("<!DOCTYPE") {
                // Skip to the matching '>', stepping over an internal subset and quotes.
                let mut depth = 0usize;
                let mut quote: Option<char> = None;
                let mut end = None;
                for (i, c) in self.rest().char_indices() {
                    match (quote, c) {
                        (Some(q), c) if c == q => quote = None,
                        (Some(_), _) => {}
                        (None, '"' | '\'') => quote = Some(c),
                        (None, '[') => depth += 1,
                        (None, ']') => depth = depth.saturating_sub(1),
                        (None, '>') if depth == 0 => {
                            end = Some(i + 1);
                            break;
                        }
                        _ => {}
                    }
                }
                match end {
                    Some(e) => self.at += e,
                    None => return self.err("the DOCTYPE is not closed"),
                }
            } else {
                return Ok(());
            }
        }
    }

    fn name(&mut self) -> Result<&'a str, XmlError> {
        let rest = self.rest();
        let len = rest
            .char_indices()
            .find(|(i, c)| {
                let start = c.is_alphabetic() || matches!(c, '_' | ':') || !c.is_ascii();
                let more = start || c.is_ascii_digit() || matches!(c, '-' | '.');
                !(if *i == 0 { start } else { more })
            })
            .map_or(rest.len(), |(i, _)| i);
        if len == 0 {
            return self.err("a name is expected");
        }
        self.at += len;
        Ok(&rest[..len])
    }

    /// Decodes character data up to (not including) `stop`.
    fn decode(&mut self, raw: &str, base: usize, attribute: bool) -> Result<String, XmlError> {
        let mut out = String::with_capacity(raw.len());
        let mut chars = raw.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            match c {
                '&' => {
                    let Some(end) = raw[i..].find(';') else {
                        self.at = base + i;
                        return self.err("a reference is not closed");
                    };
                    let entity = &raw[i + 1..i + end];
                    let decoded = match entity {
                        "lt" => Some('<'),
                        "gt" => Some('>'),
                        "amp" => Some('&'),
                        "quot" => Some('"'),
                        "apos" => Some('\''),
                        e => e
                            .strip_prefix("#x")
                            .or_else(|| e.strip_prefix("#X"))
                            .map(|h| u32::from_str_radix(h, 16))
                            .or_else(|| e.strip_prefix('#').map(|d| d.parse::<u32>()))
                            .and_then(Result::ok)
                            .and_then(char::from_u32)
                            .filter(|c| allowed(*c)),
                    };
                    match decoded {
                        Some(d) => out.push(d),
                        None => {
                            self.at = base + i;
                            return self.err(&format!("unknown reference &{entity};"));
                        }
                    }
                    while chars.peek().is_some_and(|(j, _)| *j <= i + end) {
                        chars.next();
                    }
                }
                '<' if attribute => {
                    self.at = base + i;
                    return self.err("'<' in an attribute value");
                }
                '\r' => {
                    // CR LF and a lone CR are both one line feed.
                    if chars.peek().is_some_and(|(_, n)| *n == '\n') {
                        chars.next();
                    }
                    out.push(if attribute { ' ' } else { '\n' });
                }
                '\n' | '\t' if attribute => out.push(' '),
                c => out.push(c),
            }
        }
        Ok(out)
    }

    fn element(&mut self, depth: usize) -> Result<Element, XmlError> {
        if depth > MAX_DEPTH {
            return self.err("elements are nested too deeply");
        }
        if !self.eat("<") {
            return self.err("an element is expected");
        }
        let mut element = Element::new(self.name()?);
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        loop {
            let before = self.at;
            self.skip_space();
            if self.eat("/>") {
                return Ok(element);
            }
            if self.eat(">") {
                break;
            }
            if self.at == before {
                return self.err("white space is expected between attributes");
            }
            let key = self.name()?;
            self.skip_space();
            if !self.eat("=") {
                return self.err("'=' is expected after an attribute name");
            }
            self.skip_space();
            let quote = match self.rest().chars().next() {
                Some(q @ ('"' | '\'')) => q,
                _ => return self.err("an attribute value must be quoted"),
            };
            self.at += 1;
            let Some(len) = self.rest().find(quote) else {
                return self.err("an attribute value is not closed");
            };
            let raw = &self.rest()[..len];
            let value = self.decode(raw, self.at, true)?;
            self.at += len + 1;
            // A handful of attributes are compared directly; an element with many is
            // checked through a set, so a hostile file cannot make this quadratic.
            let repeated = if element.attributes.len() < 16 {
                element.attributes.iter().any(|(k, _)| k == key)
            } else {
                if seen.is_empty() {
                    seen.extend(element.attributes.iter().map(|(k, _)| k.clone()));
                }
                !seen.insert(key.to_string())
            };
            if repeated {
                self.at = before;
                return self.err(&format!("attribute {key} appears twice"));
            }
            if element.attributes.len() >= MAX_ATTRIBUTES {
                self.at = before;
                return self.err(&format!(
                    "<{}> has more than {MAX_ATTRIBUTES} attributes",
                    element.name
                ));
            }
            element.attributes.push((key.to_string(), value));
        }
        // Content.
        let mut text = String::new();
        loop {
            let rest = self.rest();
            let Some(lt) = rest.find('<') else {
                return self.err(&format!("<{}> is not closed", element.name));
            };
            if lt > 0 {
                let decoded = self.decode(&rest[..lt], self.at, false)?;
                text.push_str(&decoded);
                self.at += lt;
            }
            if self.eat("<![CDATA[") {
                let Some(end) = self.rest().find("]]>") else {
                    return self.err("a CDATA section is not closed");
                };
                text.push_str(&self.rest()[..end].replace("\r\n", "\n").replace('\r', "\n"));
                self.at += end + 3;
            } else if self.eat("<!--") {
                self.skip_past("-->", "a comment")?;
            } else if self.eat("<?") {
                self.skip_past("?>", "a processing instruction")?;
            } else if self.eat("</") {
                let at = self.at;
                let close = self.name()?;
                if close != element.name {
                    self.at = at;
                    return self.err(&format!("</{close}> closes <{}>", element.name));
                }
                self.skip_space();
                if !self.eat(">") {
                    return self.err("'>' is expected");
                }
                if !text.is_empty() {
                    element.children.push(Node::Text(text));
                }
                return Ok(element);
            } else {
                if !text.is_empty() {
                    element.children.push(Node::Text(std::mem::take(&mut text)));
                }
                let child = self.element(depth + 1)?;
                element.children.push(Node::Element(child));
            }
        }
    }
}

/// Parses a document and returns its root element.
pub fn parse(src: &str) -> Result<Element, XmlError> {
    let mut p = Parser {
        src: src.strip_prefix('\u{FEFF}').unwrap_or(src),
        at: 0,
    };
    p.skip_misc(true)?;
    let root = p.element(0)?;
    p.skip_misc(false)?;
    if p.at != p.src.len() {
        return p.err("content after the root element");
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_usual_constructs() {
        let doc = "\u{FEFF}<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n\
            <!DOCTYPE note [ <!ENTITY x \"y > z\"> ]>\n\
            <!-- lead -->\n\
            <a:root xmlns:a='urn:x' id = \"1\" note='say \"hi\" &amp; &#x41;&#66;'>\n\
              <empty/>text &lt;one&gt;<![CDATA[<raw> & ]]]]><b k=\"v\">in</b><!-- c -->tail<?pi x?>\r\n\
            </a:root>\n<!-- after -->\n";
        let root = parse(doc).unwrap();
        assert_eq!(root.name, "a:root");
        assert_eq!(root.local_name(), "root");
        assert_eq!(root.attr("id"), Some("1"));
        assert_eq!(root.attr("note"), Some("say \"hi\" & AB"));
        assert_eq!(root.attr("missing"), None);
        let names: Vec<&str> = root.elements().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["empty", "b"]);
        assert_eq!(root.child("b").unwrap().text(), "in");
        assert_eq!(root.child("b").unwrap().attr("k"), Some("v"));
        assert_eq!(root.text(), "\ntext <one><raw> & ]]tail\n");
        assert_eq!(root.descendant("b").unwrap().text(), "in");
    }

    #[test]
    fn what_is_written_reads_back_exactly() {
        let nasty = "a<b>&\"c'\r\nline\ttab\rcr  end \u{1F600} é";
        let mut e = Element::new("r").with("v", nasty).with("empty", "");
        e.push_text(nasty);
        e.push(Element::new("kid"));
        e.push_text(" after ");
        let mut out = String::new();
        e.write(&mut out);
        assert_eq!(parse(&out).unwrap(), e);
        // Characters XML cannot carry are dropped, not written as invalid markup.
        let mut bad = Element::new("r");
        bad.push_text("a\u{1}b\u{FFFF}c");
        let mut out = String::new();
        bad.write(&mut out);
        assert_eq!(out, "<r>abc</r>");
    }

    #[test]
    fn many_attributes_are_read_in_linear_time_and_too_many_are_refused() {
        let with = |n: usize| {
            let mut doc = String::from("<a");
            for i in 0..n {
                doc.push_str(&format!(" k{i}=\"{i}\""));
            }
            doc.push_str("/>");
            doc
        };
        let e = parse(&with(MAX_ATTRIBUTES)).unwrap();
        assert_eq!(e.attributes.len(), MAX_ATTRIBUTES);
        assert_eq!(e.attr("k4095"), Some("4095"));
        // 200,000 attributes took minutes when every one was compared with all before it.
        let start = std::time::Instant::now();
        let err = parse(&with(200_000)).unwrap_err();
        assert!(
            err.to_string().contains("more than 4096 attributes"),
            "{err}"
        );
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn malformed_input_is_an_error_with_a_position() {
        for (doc, needle) in [
            ("", "an element is expected"),
            ("<a>", "is not closed"),
            ("<a></b>", "</b> closes <a>"),
            ("<a x=1/>", "must be quoted"),
            ("<a x='1' x='2'/>", "appears twice"),
            (
                "<a a0='' a1='' a2='' a3='' a4='' a5='' a6='' a7='' a8='' a9='' b0='' b1='' \
                 b2='' b3='' b4='' b5='' b6='' b7='' b8='' b9='' a3=''/>",
                "attribute a3 appears twice",
            ),
            ("<a x='1'y='2'/>", "white space is expected"),
            ("<a>&nope;</a>", "unknown reference"),
            ("<a>&#0;</a>", "unknown reference"),
            ("<a>&amp</a>", "not closed"),
            ("<a x='<'/>", "'<' in an attribute"),
            ("<a/><b/>", "content after the root"),
            ("<a/>text", "content after the root"),
            ("<a><!-- x</a>", "a comment is not closed"),
            ("<a><![CDATA[x</a>", "CDATA section is not closed"),
            ("<!DOCTYPE a [", "DOCTYPE is not closed"),
            ("< a/>", "a name is expected"),
        ] {
            let e = parse(doc).unwrap_err();
            assert!(e.message.contains(needle), "{doc:?}: {e}");
            assert!(e.offset <= doc.len());
        }
        let deep = "<a>".repeat(MAX_DEPTH + 5);
        assert!(parse(&deep).unwrap_err().message.contains("too deeply"));
    }

    #[test]
    fn line_ends_are_normalised_as_xml_requires() {
        let root = parse("<a v='x\r\ny\tz'>one\r\ntwo\rthree&#13;</a>").unwrap();
        assert_eq!(root.attr("v"), Some("x y z"));
        assert_eq!(root.text(), "one\ntwo\nthree\r");
    }
}
