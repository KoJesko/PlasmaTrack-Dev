//! Client updates (docs/UPDATES.md). A release's update manifest lists every
//! file the installer puts in the client's `bin` folder with its SHA-256 and
//! size. The manifest is signed with the release key (Ed25519, kept offline by
//! the maintainer; `scripts/make-release-key.mjs`), and clients only install
//! what carries a signature by one of [`UPDATE_KEYS`]. The server stores and
//! serves updates but cannot make one.

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};

/// Public keys whose signatures clients accept. A second key can be added
/// before the first is retired.
pub const UPDATE_KEYS: &[[u8; 32]] = &[hex32("7dd1f8d93cb66cb2f8ec86acab45add8e9c8b9bb484f77eb95ca1af7e256e073")];

/// Signed bytes are this prefix followed by the manifest text, so a signature
/// cannot be reused for anything else.
pub const SIGNING_CONTEXT: &[u8] = b"plasmatrack-update-v1\n";
pub const PRODUCT: &str = "plasmatrack";
/// Where the client asks for updates (`?platform=windows&version=0.2.0`).
pub const UPDATE_PATH: &str = "/api/v1/client/update";
/// Each file is served gzip-compressed at `{DOWNLOAD_PATH}{platform}/{version}/{path}.gz`.
pub const DOWNLOAD_PATH: &str = "/dl/releases/";
/// Largest file an update may contain (uncompressed).
pub const MAX_FILE_BYTES: u64 = 256 << 20;
pub const MAX_FILES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateManifest {
    pub product: String,
    pub platform: String,
    pub version: String,
    pub files: Vec<UpdateFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateFile {
    /// Relative to `bin`, `/`-separated: `plasmatrack.dll`, `licenses/…`.
    pub path: String,
    pub sha256: String,
    pub size: u64,
    /// Size of the `.gz` download.
    pub gz_size: u64,
}

impl UpdateManifest {
    /// R2 key / download path of one file, below [`DOWNLOAD_PATH`].
    pub fn key(&self, f: &UpdateFile) -> String {
        format!("{}/{}/{}.gz", self.platform, self.version, f.path)
    }
}

impl UpdateFile {
    /// The file's location below `dir`.
    pub fn local_path(&self, dir: &std::path::Path) -> std::path::PathBuf {
        self.path.split('/').fold(dir.to_path_buf(), |p, part| p.join(part))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UpdateError {
    #[error("signature: {0}")]
    Signature(&'static str),
    #[error("manifest: {0}")]
    Manifest(String),
}

const fn hex32(s: &str) -> [u8; 32] {
    let b = s.as_bytes();
    assert!(b.len() == 64);
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = (nibble(b[2 * i]) << 4) | nibble(b[2 * i + 1]);
        i += 1;
    }
    out
}

const fn nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => panic!("lowercase hex"),
    }
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

/// `major.minor.patch` as numbers, for ordering versions.
pub fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.split('.').map(|p| if p.is_empty() || p.len() > 9 || !p.bytes().all(|c| c.is_ascii_digit()) { None } else { p.parse().ok() });
    let out = (it.next()??, it.next()??, it.next()??);
    it.next().is_none().then_some(out)
}

fn valid_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 200
        && p.split('/').all(|part| {
            !part.is_empty() && part != "." && part != ".." && part.bytes().all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        })
}

/// Parses a manifest and checks what does not need the signature: product,
/// version format, safe and unique paths, hash format, sizes.
pub fn parse(text: &str) -> Result<UpdateManifest, UpdateError> {
    let m: UpdateManifest = serde_json::from_str(text).map_err(|e| UpdateError::Manifest(e.to_string()))?;
    let bad = |s: String| Err(UpdateError::Manifest(s));
    if m.product != PRODUCT {
        return bad(format!("product {:?}", m.product));
    }
    if !matches!(m.platform.as_str(), "windows" | "linux") {
        return bad(format!("platform {:?}", m.platform));
    }
    if parse_version(&m.version).is_none() {
        return bad(format!("version {:?}", m.version));
    }
    if m.files.is_empty() || m.files.len() > MAX_FILES {
        return bad(format!("{} files", m.files.len()));
    }
    let mut seen = std::collections::BTreeSet::new();
    for f in &m.files {
        if !valid_path(&f.path) || !seen.insert(f.path.to_ascii_lowercase()) {
            return bad(format!("path {:?}", f.path));
        }
        if f.sha256.len() != 64 || !f.sha256.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
            return bad(format!("sha256 of {}", f.path));
        }
        if f.size > MAX_FILE_BYTES || f.gz_size == 0 || f.gz_size > MAX_FILE_BYTES + (1 << 20) {
            return bad(format!("size of {}", f.path));
        }
    }
    Ok(m)
}

/// Checks `signature_hex` over the manifest text against [`UPDATE_KEYS`].
pub fn verify(text: &str, signature_hex: &str) -> Result<UpdateManifest, UpdateError> {
    verify_with(text, signature_hex, UPDATE_KEYS)
}

pub fn verify_with(text: &str, signature_hex: &str, keys: &[[u8; 32]]) -> Result<UpdateManifest, UpdateError> {
    let sig = unhex(signature_hex.trim()).ok_or(UpdateError::Signature("not hex"))?;
    let sig = Signature::from_slice(&sig).map_err(|_| UpdateError::Signature("not 64 bytes"))?;
    let msg = [SIGNING_CONTEXT, text.as_bytes()].concat();
    let ok = keys
        .iter()
        .filter_map(|k| VerifyingKey::from_bytes(k).ok())
        .any(|k| k.verify_strict(&msg, &sig).is_ok());
    if !ok {
        return Err(UpdateError::Signature("not signed by a release key"));
    }
    parse(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn manifest(version: &str, path: &str) -> String {
        serde_json::to_string(&UpdateManifest {
            product: PRODUCT.into(),
            platform: "windows".into(),
            version: version.into(),
            files: vec![UpdateFile { path: path.into(), sha256: "ab".repeat(32), size: 10, gz_size: 30 }],
        })
        .unwrap()
    }

    fn sign(key: &SigningKey, text: &str) -> String {
        let sig = key.sign(&[SIGNING_CONTEXT, text.as_bytes()].concat());
        sig.to_bytes().iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn accepts_only_signed_manifests() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let other = SigningKey::from_bytes(&[8; 32]);
        let keys = [key.verifying_key().to_bytes()];
        let text = manifest("0.2.0", "licenses/LLVM-LICENSE.txt");
        let m = verify_with(&text, &sign(&key, &text), &keys).unwrap();
        assert_eq!(m.key(&m.files[0]), "windows/0.2.0/licenses/LLVM-LICENSE.txt.gz");
        assert_eq!(verify_with(&text, &sign(&other, &text), &keys), Err(UpdateError::Signature("not signed by a release key")));
        let changed = text.replace("0.2.0", "0.2.1");
        assert!(verify_with(&changed, &sign(&key, &text), &keys).is_err(), "any change breaks it");
        // Signed without the context prefix: refused.
        let bare: String = key.sign(text.as_bytes()).to_bytes().iter().map(|b| format!("{b:02x}")).collect();
        assert!(verify_with(&text, &bare, &keys).is_err());
        assert!(verify_with(&text, "zz", &keys).is_err());
        // The shipped key is a valid curve point.
        assert!(VerifyingKey::from_bytes(&UPDATE_KEYS[0]).is_ok());
    }

    #[test]
    fn refuses_unsafe_paths_and_bad_versions() {
        for p in ["../x.dll", "a//b", "/abs.exe", "c:\\x.exe", "a/./b", ""] {
            assert!(parse(&manifest("0.2.0", p)).is_err(), "{p}");
        }
        for v in ["0.2", "0.2.0.1", "v0.2.0", "0.2.x", "0..2"] {
            assert!(parse(&manifest(v, "x.exe")).is_err(), "{v}");
        }
        assert!(parse(&manifest("0.2.0", "plasmatrack-clientw.exe")).is_ok());
        assert!(parse(&manifest("0.2.0", "x.exe").replace("\"files\"", "\"extra\":1,\"files\"")).is_err(), "unknown fields");
    }

    #[test]
    fn orders_versions() {
        assert!(parse_version("0.10.0") > parse_version("0.9.9"));
        assert!(parse_version("1.0.0") > parse_version("0.99.99"));
        assert_eq!(parse_version("0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_version("0.2.0-rc1"), None);
    }
}
