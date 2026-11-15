//! XML for WebDAV, CalDAV and CardDAV.
//!
//! Requests are read with `roxmltree` with DTDs off and a node limit, so entity tricks and
//! giant documents are refused. Responses are written by hand: everything that comes from a
//! database or a client goes through [`esc`], and names from a request are re-checked
//! before being echoed back.

use std::fmt::Write as _;

pub const NS_DAV: &str = "DAV:";
pub const NS_CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
pub const NS_CARDDAV: &str = "urn:ietf:params:xml:ns:carddav";
pub const NS_CS: &str = "http://calendarserver.org/ns/";
pub const NS_ICAL: &str = "http://apple.com/ns/ical/";

/// Most nodes a request document may have.
const MAX_NODES: u32 = 20_000;

/// Escapes text for element content and attribute values. Characters XML 1.0 cannot carry
/// at all (most control characters) are dropped.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' => out.push(c),
            // A raw CR is read back as LF by XML parsers, which would break CRLF line ends.
            '\r' => out.push_str("&#13;"),
            c if (c as u32) < 0x20 || c == '\u{FFFE}' || c == '\u{FFFF}' => {}
            c => out.push(c),
        }
    }
    out
}

/// A property named in a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropName {
    pub ns: String,
    pub local: String,
}

impl PropName {
    pub fn new(ns: &str, local: &str) -> Self {
        Self {
            ns: ns.to_owned(),
            local: local.to_owned(),
        }
    }

    pub fn is(&self, ns: &str, local: &str) -> bool {
        self.ns == ns && self.local == local
    }
}

/// What a PROPFIND asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropRequest {
    All,
    Names,
    Props(Vec<PropName>),
}

/// Parses an XML document with hard limits.
pub fn parse(body: &[u8]) -> Result<roxmltree::Document<'_>, String> {
    let text = std::str::from_utf8(body).map_err(|_| "the request is not UTF-8".to_owned())?;
    let options = roxmltree::ParsingOptions {
        allow_dtd: false,
        nodes_limit: MAX_NODES,
        ..roxmltree::ParsingOptions::default()
    };
    roxmltree::Document::parse_with_options(text, options)
        .map_err(|e| format!("the request is not valid XML: {e}"))
}

/// The properties listed inside a `<prop>` element.
pub fn prop_names(node: roxmltree::Node<'_, '_>) -> Vec<PropName> {
    node.children()
        .filter(roxmltree::Node::is_element)
        .map(|p| PropName::new(p.tag_name().namespace().unwrap_or(""), p.tag_name().name()))
        .collect()
}

/// The first child element of `node` that is `{ns}local`.
pub fn child<'a, 'i>(
    node: roxmltree::Node<'a, 'i>,
    ns: &str,
    local: &str,
) -> Option<roxmltree::Node<'a, 'i>> {
    node.children().find(|c| {
        c.is_element() && c.tag_name().namespace() == Some(ns) && c.tag_name().name() == local
    })
}

/// Reads a PROPFIND body. An empty body means "all properties".
pub fn parse_propfind(body: &[u8]) -> Result<PropRequest, String> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(PropRequest::All);
    }
    let doc = parse(body)?;
    let root = doc.root_element();
    if !(root.tag_name().namespace() == Some(NS_DAV) && root.tag_name().name() == "propfind") {
        return Err("expected a DAV:propfind request".into());
    }
    if child(root, NS_DAV, "propname").is_some() {
        return Ok(PropRequest::Names);
    }
    if let Some(p) = child(root, NS_DAV, "prop") {
        return Ok(PropRequest::Props(prop_names(p)));
    }
    Ok(PropRequest::All)
}

/// The XML prefix used for a namespace in responses.
fn prefix(ns: &str) -> Option<&'static str> {
    match ns {
        NS_DAV => Some("D"),
        NS_CALDAV => Some("C"),
        NS_CARDDAV => Some("A"),
        NS_CS => Some("CS"),
        NS_ICAL => Some("I"),
        _ => None,
    }
}

/// `<D:name>inner</D:name>` for a known namespace; for an unknown one, with its own declaration.
pub fn element(ns: &str, local: &str, inner: &str) -> String {
    match prefix(ns) {
        Some(p) if inner.is_empty() => format!("<{p}:{local}/>"),
        Some(p) => format!("<{p}:{local}>{inner}</{p}:{local}>"),
        None if inner.is_empty() => format!("<x:{local} xmlns:x=\"{}\"/>", esc(ns)),
        None => format!("<x:{local} xmlns:x=\"{}\">{inner}</x:{local}>", esc(ns)),
    }
}

/// A `<D:href>`.
pub fn href(path: &str) -> String {
    format!("<D:href>{}</D:href>", esc(path))
}

/// Percent-encodes one path segment.
pub fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'@' | b'=' | b'+')
        {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Decodes `%XX` escapes. `None` for a broken escape or bytes that are not UTF-8.
pub fn decode_segment(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Builds a `207 Multi-Status` body.
pub struct Multistatus {
    body: String,
}

impl Multistatus {
    pub fn new() -> Self {
        Self {
            body: String::new(),
        }
    }

    /// One resource with the properties found (already written as XML) and those that are not.
    pub fn response(&mut self, href_path: &str, found: &[String], missing: &[PropName]) {
        let _ = write!(self.body, "<D:response>{}", href(href_path));
        if !found.is_empty() {
            let _ = write!(
                self.body,
                "<D:propstat><D:prop>{}</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>",
                found.concat()
            );
        }
        if !missing.is_empty() {
            let props: String = missing
                .iter()
                .map(|p| element(&p.ns, &p.local, ""))
                .collect();
            let _ = write!(self.body, "<D:propstat><D:prop>{props}</D:prop><D:status>HTTP/1.1 404 Not Found</D:status></D:propstat>");
        }
        if found.is_empty() && missing.is_empty() {
            self.body
                .push_str("<D:propstat><D:prop/><D:status>HTTP/1.1 200 OK</D:status></D:propstat>");
        }
        self.body.push_str("</D:response>");
    }

    /// One resource with properties grouped by status line, such as `HTTP/1.1 200 OK`.
    pub fn response_grouped(&mut self, href_path: &str, groups: &[(&str, String)]) {
        let _ = write!(self.body, "<D:response>{}", href(href_path));
        for (status_line, props) in groups {
            let _ = write!(self.body, "<D:propstat><D:prop>{props}</D:prop><D:status>{status_line}</D:status></D:propstat>");
        }
        self.body.push_str("</D:response>");
    }

    /// One resource that has only a status (a deleted object in a sync report, or a missing one).
    pub fn status(&mut self, href_path: &str, status_line: &str) {
        let _ = write!(
            self.body,
            "<D:response>{}<D:status>{status_line}</D:status></D:response>",
            href(href_path)
        );
    }

    /// Finishes the document, optionally with a sync token.
    pub fn finish(self, sync_token: Option<&str>) -> String {
        let token = sync_token
            .map(|t| format!("<D:sync-token>{}</D:sync-token>", esc(t)))
            .unwrap_or_default();
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:multistatus xmlns:D=\"{NS_DAV}\" xmlns:C=\"{NS_CALDAV}\" xmlns:A=\"{NS_CARDDAV}\" xmlns:CS=\"{NS_CS}\" xmlns:I=\"{NS_ICAL}\">{}{token}</D:multistatus>",
            self.body
        )
    }
}

/// A DAV error body such as `<D:error><D:valid-sync-token/></D:error>`.
pub fn error_body(inner: &str) -> String {
    format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:error xmlns:D=\"{NS_DAV}\" xmlns:C=\"{NS_CALDAV}\" xmlns:A=\"{NS_CARDDAV}\">{inner}</D:error>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_escaped_and_unwritable_characters_dropped() {
        assert_eq!(esc("a<b>&\"c'd"), "a&lt;b&gt;&amp;&quot;c&apos;d");
        assert_eq!(esc("ok\u{0}\u{8}\u{b}\u{1f}\t\n"), "ok\t\n");
        assert_eq!(
            esc("a\r\nb"),
            "a&#13;\nb",
            "CR survives an XML parser as CR"
        );
        let xml = format!("<a>{}</a>", esc("x\r\ny"));
        assert_eq!(
            roxmltree::Document::parse(&xml)
                .unwrap()
                .root_element()
                .text(),
            Some("x\r\ny")
        );
        assert_eq!(esc("</D:href><script>"), "&lt;/D:href&gt;&lt;script&gt;");
    }

    #[test]
    fn path_segments_round_trip_and_reject_garbage() {
        for s in ["alice@example.com", "a b/c", "über.ics", "100%.ics", "x\ny"] {
            assert_eq!(
                decode_segment(&encode_segment(s)).as_deref(),
                Some(s),
                "{s}"
            );
        }
        assert_eq!(encode_segment("alice@example.com"), "alice@example.com");
        assert_eq!(encode_segment("a b"), "a%20b");
        assert_eq!(
            decode_segment("alice%40example.com").as_deref(),
            Some("alice@example.com")
        );
        for bad in ["%", "%4", "%zz", "%ff%fe"] {
            assert_eq!(decode_segment(bad), None, "{bad}");
        }
    }

    #[test]
    fn propfind_bodies_are_understood() {
        assert_eq!(parse_propfind(b"").unwrap(), PropRequest::All);
        assert_eq!(parse_propfind(b"  \n").unwrap(), PropRequest::All);
        let all = br#"<?xml version="1.0"?><D:propfind xmlns:D="DAV:"><D:allprop/></D:propfind>"#;
        assert_eq!(parse_propfind(all).unwrap(), PropRequest::All);
        let names = br#"<propfind xmlns="DAV:"><propname/></propfind>"#;
        assert_eq!(parse_propfind(names).unwrap(), PropRequest::Names);
        let props = br#"<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><d:displayname/><c:calendar-home-set/><x:odd xmlns:x="urn:x"/></d:prop></d:propfind>"#;
        assert_eq!(
            parse_propfind(props).unwrap(),
            PropRequest::Props(vec![
                PropName::new(NS_DAV, "displayname"),
                PropName::new(NS_CALDAV, "calendar-home-set"),
                PropName::new("urn:x", "odd")
            ]),
            "any prefix works; the namespace counts"
        );
    }

    #[test]
    fn hostile_xml_is_refused() {
        assert!(parse_propfind(b"not xml").is_err());
        assert!(parse_propfind(br#"<other xmlns="DAV:"/>"#).is_err());
        assert!(parse_propfind(&[0xff, 0xfe]).is_err());
        let dtd = br#"<?xml version="1.0"?><!DOCTYPE x [<!ENTITY a "aaaaaaaaaa"><!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;">]><D:propfind xmlns:D="DAV:"><D:prop>&b;</D:prop></D:propfind>"#;
        assert!(
            parse_propfind(dtd).is_err(),
            "DTDs and entity expansion are not allowed"
        );
        let wide = format!(
            "<D:propfind xmlns:D=\"DAV:\"><D:prop>{}</D:prop></D:propfind>",
            "<D:a/>".repeat(30_000)
        );
        assert!(
            parse_propfind(wide.as_bytes()).is_err(),
            "a huge document is refused"
        );
    }

    #[test]
    fn multistatus_is_well_formed_whatever_the_names() {
        let mut m = Multistatus::new();
        m.response(
            "/dav/calendars/a@b.de/x\"<y/",
            &[element(NS_DAV, "displayname", &esc("A & B <c>"))],
            &[
                PropName::new("urn:evil\"><x", "weird"),
                PropName::new(NS_DAV, "nothing"),
            ],
        );
        m.status("/dav/gone.ics", "HTTP/1.1 404 Not Found");
        let xml = m.finish(Some("urn:rustmail:sync:7"));
        let doc = roxmltree::Document::parse(&xml).expect("well-formed");
        assert_eq!(
            doc.descendants()
                .filter(|n| n.tag_name().name() == "response")
                .count(),
            2
        );
        assert!(xml.contains("A &amp; B &lt;c&gt;") && xml.contains("urn:rustmail:sync:7"));
        assert!(xml.contains("404 Not Found"));
    }

    #[test]
    fn a_resource_with_no_properties_still_gets_a_status() {
        let mut m = Multistatus::new();
        m.response("/x", &[], &[]);
        let xml = m.finish(None);
        assert!(roxmltree::Document::parse(&xml).is_ok() && xml.contains("200 OK"));
    }
}
