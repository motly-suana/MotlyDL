//! Metalink files (.meta4 and .metalink): one file, several mirrors, and the
//! file's checksum. Linux distributions and many projects publish them.

use anyhow::{Context, Result, bail};

#[derive(Debug, PartialEq)]
pub struct Metalink {
    pub name: String,
    pub size: Option<u64>,
    /// Strongest supported checksum as hex (SHA-512 > SHA-256 > SHA-1 > MD5).
    pub checksum: Option<String>,
    /// Best first.
    pub urls: Vec<String>,
}

pub fn is_metalink(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
    path.ends_with(".meta4") || path.ends_with(".metalink")
}

pub fn parse(xml: &str) -> Result<Metalink> {
    let doc = roxmltree::Document::parse(xml).context("not a valid Metalink file")?;
    let file = doc.descendants().find(|n| n.has_tag_name("file")).context("the Metalink lists no file")?;
    let name = file.attribute("name").context("the Metalink file has no name")?.to_string();
    let text = |tag: &str| file.descendants().find(|n| n.has_tag_name(tag)).and_then(|n| n.text()).map(|t| t.trim().to_string());
    let size = text("size").and_then(|s| s.parse().ok());
    let checksum = file
        .descendants()
        .filter(|n| n.has_tag_name("hash"))
        .filter_map(|n| {
            let algo = n.attribute("type").and_then(crate::checksum::Algo::from_metalink_type)?;
            let hex = n.text()?.trim().to_ascii_lowercase();
            (!hex.is_empty()).then_some((algo, hex))
        })
        .max_by_key(|(algo, _)| *algo)
        .map(|(_, hex)| hex);
    // v4: `priority` (1 is best). v3: `preference` (100 is best).
    let mut urls: Vec<(i64, String)> = file
        .descendants()
        .filter(|n| n.has_tag_name("url"))
        .filter_map(|n| {
            let u = n.text()?.trim().to_string();
            if !(u.starts_with("http://") || u.starts_with("https://")) {
                return None;
            }
            let rank = match (n.attribute("priority"), n.attribute("preference")) {
                (Some(p), _) => p.parse::<i64>().unwrap_or(999_999),
                (None, Some(p)) => 1000 - p.parse::<i64>().unwrap_or(0),
                _ => 999_999,
            };
            Some((rank, u))
        })
        .collect();
    if urls.is_empty() {
        bail!("the Metalink has no web addresses MotlyDL can use");
    }
    urls.sort_by_key(|(rank, _)| *rank); // stable: equal ranks keep their order
    Ok(Metalink { name, size, checksum, urls: urls.into_iter().map(|(_, u)| u).collect() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_version_4() {
        let m = parse(r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="x.iso"><size>123</size>
            <hash type="sha-1">aa</hash><hash type="sha-256">ABCD</hash>
            <url priority="2">https://b.example/x.iso</url><url priority="1">https://a.example/x.iso</url><url>ftp://c.example/x.iso</url></file></metalink>"#).unwrap();
        assert_eq!(m, Metalink { name: "x.iso".into(), size: Some(123), checksum: Some("abcd".into()), urls: vec!["https://a.example/x.iso".into(), "https://b.example/x.iso".into()] });
    }

    #[test]
    fn reads_version_3() {
        let m = parse(r#"<metalink version="3.0" xmlns="http://www.metalinker.org/"><files><file name="y.zip"><size>9</size>
            <verification><hash type="sha256">ff00</hash></verification>
            <resources><url type="http" preference="50">http://slow.example/y.zip</url><url type="https" preference="100">https://fast.example/y.zip</url></resources></file></files></metalink>"#).unwrap();
        assert_eq!(m.urls[0], "https://fast.example/y.zip");
        assert_eq!(m.checksum.as_deref(), Some("ff00"));
        assert_eq!(m.size, Some(9));
    }

    #[test]
    fn accepts_md5_sha1_and_prefers_strongest() {
        let m_md5 = parse(r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="a.bin"><hash type="md5">900150983CD24FB0D6963F7D28E17F72</hash><url>https://a.example/a.bin</url></file></metalink>"#).unwrap();
        assert_eq!(m_md5.checksum.as_deref(), Some("900150983cd24fb0d6963f7d28e17f72"));

        let m_sha1 = parse(r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="b.bin"><hash type="sha1">A9993E364706816ABA3E25717850C26C9CD0D89D</hash><url>https://a.example/b.bin</url></file></metalink>"#).unwrap();
        assert_eq!(m_sha1.checksum.as_deref(), Some("a9993e364706816aba3e25717850c26c9cd0d89d"));

        let m_512 = parse(r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="c.bin"><hash type="md5">aaaa</hash><hash type="sha-256">bbbb</hash><hash type="sha-512">CCCC</hash><url>https://a.example/c.bin</url></file></metalink>"#).unwrap();
        assert_eq!(m_512.checksum.as_deref(), Some("cccc"));
    }

    #[test]
    fn spots_metalink_links() {
        assert!(is_metalink("https://x/a.iso.meta4?mirror=1") && is_metalink("https://x/a.METALINK") && !is_metalink("https://x/a.iso"));
        assert!(parse("<metalink/>").is_err());
    }
}
