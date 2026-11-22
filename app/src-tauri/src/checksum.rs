//! Streaming file checksums (MD5, SHA-1, SHA-256, SHA-512) and sidecar
//! file auto-detection (.md5, .sha1, .sha256, .sha512).
//!
//! Files can exceed 10 GB, so hashing always streams in fixed 1 MiB chunks.

use md5::{Digest, Md5};
use sha1::Sha1;
use sha2::{Sha256, Sha512};
use std::io::Read;
use std::path::Path;

const MAX_SIDECAR_BYTES: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Algo {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

impl Algo {
    pub fn label(self) -> &'static str {
        match self {
            Self::Md5 => "MD5",
            Self::Sha1 => "SHA-1",
            Self::Sha256 => "SHA-256",
            Self::Sha512 => "SHA-512",
        }
    }

    /// Detects the algorithm by trimmed hex digest length (32/40/64/128).
    pub fn from_hex(raw: &str) -> Option<(Self, String)> {
        let s = raw.trim();
        if !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let algo = match s.len() {
            32 => Self::Md5,
            40 => Self::Sha1,
            64 => Self::Sha256,
            128 => Self::Sha512,
            _ => return None,
        };
        Some((algo, s.to_ascii_lowercase()))
    }

    pub fn from_metalink_type(kind: &str) -> Option<Self> {
        match kind.trim().to_ascii_lowercase().as_str() {
            "md5" => Some(Self::Md5),
            "sha-1" | "sha1" => Some(Self::Sha1),
            "sha-256" | "sha256" => Some(Self::Sha256),
            "sha-512" | "sha512" => Some(Self::Sha512),
            _ => None,
        }
    }
}

fn stream_digest<D: Digest>(file: &mut std::fs::File) -> std::io::Result<String> {
    let mut hasher = D::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Computes `algo` over `path` in streaming 1 MiB chunks.
pub fn hash_file(path: &Path, algo: Algo) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    match algo {
        Algo::Md5 => stream_digest::<Md5>(&mut file),
        Algo::Sha1 => stream_digest::<Sha1>(&mut file),
        Algo::Sha256 => stream_digest::<Sha256>(&mut file),
        Algo::Sha512 => stream_digest::<Sha512>(&mut file),
    }
}

/// Parses a sidecar checksum file (GNU `hash  filename`, BSD `ALGO (name) = hash`,
/// or a bare hex digest). When multiple entries exist, matches `target_name`.
pub fn parse_sidecar_text(text: &str, target_name: &str, expected_algo: Algo) -> Option<(Algo, String)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let target_base = Path::new(target_name).file_name().and_then(|n| n.to_str()).unwrap_or(target_name);
    let mut entries: Vec<(Algo, String, Option<String>)> = Vec::new();

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        // BSD format: SHA256 (file.iso) = <hex>
        if let Some((left, right)) = line.split_once('=') {
            let right_tok = right.split_whitespace().next().unwrap_or("");
            if let Some((algo, hex)) = Algo::from_hex(right_tok) {
                let fname = left
                    .split_once('(')
                    .and_then(|(_, rest)| rest.rsplit_once(')'))
                    .map(|(inside, _)| inside.trim().to_string());
                entries.push((algo, hex, fname));
                continue;
            }
        }
        // GNU coreutils or bare hex: <hex> [*?filename]
        let mut parts = line.splitn(2, char::is_whitespace);
        let first = parts.next().unwrap_or("");
        if let Some((algo, hex)) = Algo::from_hex(first) {
            let fname = parts
                .next()
                .map(|rest| rest.trim_start().trim_start_matches('*').trim().to_string())
                .filter(|s| !s.is_empty());
            entries.push((algo, hex, fname));
        }
    }

    if entries.is_empty() {
        return None;
    }

    let name_matches = |entry_name: &str, case_insensitive: bool| -> bool {
        let base = entry_name.rsplit(['/', '\\']).next().unwrap_or(entry_name);
        if case_insensitive {
            base.eq_ignore_ascii_case(target_base)
        } else {
            base == target_base
        }
    };

    // Exact filename match (preferring expected_algo, then any supported algo).
    for insensitive in [false, true] {
        if let Some((algo, hex, _)) = entries
            .iter()
            .find(|(a, _, f)| *a == expected_algo && f.as_deref().is_some_and(|n| name_matches(n, insensitive)))
        {
            return Some((*algo, hex.clone()));
        }
        if let Some((algo, hex, _)) = entries
            .iter()
            .find(|(_, _, f)| f.as_deref().is_some_and(|n| name_matches(n, insensitive)))
        {
            return Some((*algo, hex.clone()));
        }
    }

    // Only fall back when the sidecar describes a single file (renamed downloads)
    // or carries entries without filenames; multi-file SUMS files with no name match
    // must not verify against the wrong file.
    if entries.len() == 1 || entries.iter().all(|(_, _, f)| f.is_none()) {
        if let Some((algo, hex, _)) = entries.iter().find(|(a, _, _)| *a == expected_algo) {
            return Some((*algo, hex.clone()));
        }
        return entries.into_iter().next().map(|(a, h, _)| (a, h));
    }

    None
}

/// Searches next to `file_path` for a matching `.sha512`, `.sha256`, `.sha1`, or `.md5`
/// sidecar file (strongest first, `<filename>.<ext>` before `<stem>.<ext>`).
pub fn find_sidecar(file_path: &Path) -> Option<(Algo, String)> {
    let dir = file_path.parent()?;
    let filename = file_path.file_name()?.to_str()?;
    let stem = file_path.file_stem().and_then(|s| s.to_str()).unwrap_or(filename);

    const CANDIDATES: [(Algo, &[&str]); 4] = [
        (Algo::Sha512, &["sha512", "sha512sum"]),
        (Algo::Sha256, &["sha256", "sha256sum"]),
        (Algo::Sha1, &["sha1", "sha1sum"]),
        (Algo::Md5, &["md5", "md5sum"]),
    ];

    for (algo, exts) in CANDIDATES {
        for ext in exts {
            let mut paths = vec![dir.join(format!("{filename}.{ext}"))];
            if stem != filename {
                paths.push(dir.join(format!("{stem}.{ext}")));
            }
            for cand in paths {
                let Ok(meta) = std::fs::metadata(&cand) else { continue };
                if !meta.is_file() || meta.len() > MAX_SIDECAR_BYTES {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&cand) else { continue };
                if let Some(parsed) = parse_sidecar_text(&text, filename, algo) {
                    return Some(parsed);
                }
            }
        }
    }
    None
}

/// Verifies `path` against `expected_raw` (if non-empty) or an adjacent sidecar file.
/// Returns `Ok(None)` when no checksum and no sidecar exist.
pub fn verify_file(path: &Path, expected_raw: Option<&str>) -> std::result::Result<Option<(Algo, String)>, String> {
    let target = match expected_raw.map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => match Algo::from_hex(raw) {
            Some(parsed) => parsed,
            None => {
                return Err("Unsupported checksum format. Expected 32 (MD5), 40 (SHA-1), 64 (SHA-256), or 128 (SHA-512) hex characters.".into());
            }
        },
        None => match find_sidecar(path) {
            Some(parsed) => parsed,
            None => return Ok(None),
        },
    };

    let (algo, want) = target;
    let got = hash_file(path, algo).map_err(|_| "The file finished, but couldn't be read to check it.".to_string())?;
    if got.eq_ignore_ascii_case(&want) {
        Ok(Some((algo, want)))
    } else {
        Err(format!(
            "The file's {} checksum doesn't match. Expected {want}, got {got}. It may be corrupted or not the file you meant.",
            algo.label()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("motlydl-checksum-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn hashes_abc_with_all_supported_algorithms() {
        let dir = temp_dir("abc");
        let p = dir.join("abc.txt");
        std::fs::write(&p, b"abc").unwrap();

        assert_eq!(hash_file(&p, Algo::Md5).unwrap(), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(hash_file(&p, Algo::Sha1).unwrap(), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(hash_file(&p, Algo::Sha256).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            hash_file(&p, Algo::Sha512).unwrap(),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn verify_reports_algorithm_expected_and_computed_on_mismatch() {
        let dir = temp_dir("mismatch");
        let p = dir.join("abc.txt");
        std::fs::write(&p, b"abc").unwrap();

        let err = verify_file(&p, Some("00000000000000000000000000000000")).unwrap_err();
        assert!(err.contains("MD5"), "{err}");
        assert!(err.contains("00000000000000000000000000000000"), "{err}");
        assert!(err.contains("900150983cd24fb0d6963f7d28e17f72"), "{err}");

        // Uppercase input compares case-insensitively.
        assert_eq!(
            verify_file(&p, Some("  900150983CD24FB0D6963F7D28E17F72  ")).unwrap(),
            Some((Algo::Md5, "900150983cd24fb0d6963f7d28e17f72".into()))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn auto_detects_and_parses_sidecar_files() {
        let dir = temp_dir("sidecar");
        let file = dir.join("release.tar.gz");
        std::fs::write(&file, b"abc").unwrap();

        // Stem-named .md5 sidecar
        let md5_sidecar = dir.join("release.tar.md5");
        std::fs::write(&md5_sidecar, "\u{feff}900150983CD24FB0D6963F7D28E17F72 *release.tar.gz\n").unwrap();
        assert_eq!(
            verify_file(&file, None).unwrap(),
            Some((Algo::Md5, "900150983cd24fb0d6963f7d28e17f72".into()))
        );

        // Stronger .sha256 sidecar alongside .md5 takes precedence; multi-line GNU format selects matching file
        let sha256_sidecar = dir.join("release.tar.gz.sha256");
        std::fs::write(
            &sha256_sidecar,
            "0000000000000000000000000000000000000000000000000000000000000000  other.zip\n\
             ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  ./release.tar.gz\n",
        )
        .unwrap();
        assert_eq!(
            verify_file(&file, None).unwrap(),
            Some((Algo::Sha256, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into()))
        );

        // Multi-line sidecar with no matching filename does not pick an unrelated file's hash
        std::fs::write(
            &sha256_sidecar,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  one.zip\n\
             bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  two.zip\n",
        )
        .unwrap();
        assert_eq!(
            parse_sidecar_text(&std::fs::read_to_string(&sha256_sidecar).unwrap(), "release.tar.gz", Algo::Sha256),
            None
        );

        // BSD format
        assert_eq!(
            parse_sidecar_text("SHA1 (release.tar.gz) = A9993E364706816ABA3E25717850C26C9CD0D89D", "release.tar.gz", Algo::Sha1),
            Some((Algo::Sha1, "a9993e364706816aba3e25717850c26c9cd0d89d".into()))
        );

        let _ = std::fs::remove_dir_all(dir);
    }
}
