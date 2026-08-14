//! Safe, user-approved release discovery and replacement primitives for Gocode.

use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::Command,
};

use reqwest::Url;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const RELEASES_URL: &str = "https://api.github.com/repos/gocode/gocode/releases";
pub const WINDOWS_ARCHIVE_SUFFIX: &str = "windows-x86_64.zip";
pub const CHECKSUM_ASSET: &str = "SHA256SUMS";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub name: String,
    pub download_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: Version,
    pub notes: String,
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    pub version: Version,
    pub notes: String,
    pub archive: ReleaseAsset,
    pub checksums: ReleaseAsset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    Network(String),
    InvalidRelease(String),
    AssetNotFound,
    ChecksumMissing,
    ChecksumMismatch,
    UnsafeArchive(String),
    Io(String),
    Replace(String),
    Rollback(String),
    UnsupportedPlatform,
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(error)
            | Self::InvalidRelease(error)
            | Self::UnsafeArchive(error)
            | Self::Io(error)
            | Self::Replace(error)
            | Self::Rollback(error) => f.write_str(error),
            Self::AssetNotFound => {
                f.write_str("The release does not contain the required Windows update files.")
            }
            Self::ChecksumMissing => {
                f.write_str("The release does not provide a checksum for the update archive.")
            }
            Self::ChecksumMismatch => f.write_str("Gocode could not verify the downloaded update."),
            Self::UnsupportedPlatform => {
                f.write_str("Automatic updates are currently available only on Windows.")
            }
        }
    }
}
impl std::error::Error for UpdateError {}
impl From<io::Error> for UpdateError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

/// Parses release metadata from GitHub and selects only a newer, stable Windows release.
#[must_use]
pub fn available_update(
    current: &Version,
    releases: impl IntoIterator<Item = ReleaseInfo>,
) -> Option<UpdateInfo> {
    releases
        .into_iter()
        .filter(|release| release.version > *current)
        .max_by(|a, b| a.version.cmp(&b.version))
        .and_then(|release| {
            let prefix = format!("gocode-{}-", release.version);
            let archive = release
                .assets
                .iter()
                .find(|asset| asset.name == format!("{prefix}{WINDOWS_ARCHIVE_SUFFIX}"))
                .cloned()?;
            let checksums = release
                .assets
                .iter()
                .find(|asset| asset.name == CHECKSUM_ASSET)
                .cloned()?;
            Some(UpdateInfo {
                version: release.version,
                notes: release.notes,
                archive,
                checksums,
            })
        })
}

/// A GitHub REST client deliberately limited to the public release endpoint.
pub struct GitHubReleaseSource {
    client: reqwest::Client,
    endpoint: Url,
}
impl GitHubReleaseSource {
    /// # Errors
    ///
    /// Returns an error when the release endpoint client cannot be constructed.
    pub fn new() -> Result<Self, UpdateError> {
        Self::with_endpoint(RELEASES_URL)
    }
    /// # Errors
    ///
    /// Returns an error when the endpoint URL or HTTP client is invalid.
    pub fn with_endpoint(endpoint: &str) -> Result<Self, UpdateError> {
        Ok(Self {
            client: reqwest::Client::builder()
                .user_agent("gocode-updater")
                .build()
                .map_err(|e| UpdateError::Network(e.to_string()))?,
            endpoint: Url::parse(endpoint).map_err(|e| UpdateError::Network(e.to_string()))?,
        })
    }
    /// # Errors
    ///
    /// Returns an error when GitHub cannot be reached or its release metadata is invalid.
    pub async fn stable_releases(&self) -> Result<Vec<ReleaseInfo>, UpdateError> {
        let releases: Vec<GitHubRelease> = self
            .client
            .get(self.endpoint.clone())
            .send()
            .await
            .map_err(|e| UpdateError::Network(e.to_string()))?
            .error_for_status()
            .map_err(|e| UpdateError::Network(e.to_string()))?
            .json()
            .await
            .map_err(|e| UpdateError::Network(e.to_string()))?;
        releases
            .into_iter()
            .filter(|release| !release.draft && !release.prerelease)
            .map(TryInto::try_into)
            .collect()
    }
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    body: Option<String>,
    draft: bool,
    prerelease: bool,
    assets: Vec<GitHubAsset>,
}
#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}
impl TryFrom<GitHubRelease> for ReleaseInfo {
    type Error = UpdateError;
    fn try_from(value: GitHubRelease) -> Result<Self, Self::Error> {
        let version = Version::parse(value.tag_name.trim_start_matches('v')).map_err(|e| {
            UpdateError::InvalidRelease(format!("invalid release tag {}: {e}", value.tag_name))
        })?;
        Ok(Self {
            version,
            notes: value.body.unwrap_or_default(),
            assets: value
                .assets
                .into_iter()
                .map(|a| ReleaseAsset {
                    name: a.name,
                    download_url: a.browser_download_url,
                })
                .collect(),
        })
    }
}

/// Rejects non-HTTPS URLs before a download is attempted.
///
/// # Errors
///
/// Returns an error for invalid or non-HTTPS URLs.
pub fn official_download_url(url: &str) -> Result<Url, UpdateError> {
    let url = Url::parse(url).map_err(|e| UpdateError::Network(e.to_string()))?;
    if url.scheme() == "https" {
        Ok(url)
    } else {
        Err(UpdateError::Network(
            "official updates require HTTPS".into(),
        ))
    }
}

/// Stores a fully downloaded artifact only after the partial file has been completed.
///
/// # Errors
///
/// Returns an error for invalid URLs, failed requests, or staging I/O failures.
pub async fn download_to_staging(
    client: &reqwest::Client,
    url: &str,
    staging: &Path,
) -> Result<PathBuf, UpdateError> {
    let url = official_download_url(url)?;
    fs::create_dir_all(staging)?;
    let partial = staging.join("download.partial");
    let final_path = staging.join("release.zip");
    let result = async {
        let bytes = client
            .get(url)
            .send()
            .await
            .map_err(|e| UpdateError::Network(e.to_string()))?
            .error_for_status()
            .map_err(|e| UpdateError::Network(e.to_string()))?
            .bytes()
            .await
            .map_err(|e| UpdateError::Network(e.to_string()))?;
        tokio::fs::write(&partial, bytes)
            .await
            .map_err(|e| UpdateError::Io(e.to_string()))?;
        tokio::fs::rename(&partial, &final_path)
            .await
            .map_err(|e| UpdateError::Io(e.to_string()))?;
        Ok(final_path.clone())
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(partial);
    }
    result
}

/// # Errors
///
/// Returns an error when the requested asset has no valid SHA-256 checksum.
pub fn checksum_for(checksums: &str, asset_name: &str) -> Result<String, UpdateError> {
    checksums
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            let hash = fields.next()?;
            let name = fields.next()?.trim_start_matches('*');
            (name == asset_name).then_some(hash.to_ascii_lowercase())
        })
        .filter(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or(UpdateError::ChecksumMissing)
}

/// # Errors
///
/// Returns an error when the file cannot be read or its digest does not match.
pub fn verify_sha256(path: &Path, expected: &str) -> Result<(), UpdateError> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    (hex::encode(hasher.finalize()) == expected.to_ascii_lowercase())
        .then_some(())
        .ok_or(UpdateError::ChecksumMismatch)
}

/// Extracts the two exact executable names only; every archive path must be a root-level file.
///
/// # Errors
///
/// Returns an error for malformed archives, unsafe entries, missing executables, or I/O failure.
pub fn extract_windows_archive(
    archive: &Path,
    staging: &Path,
) -> Result<(PathBuf, PathBuf), UpdateError> {
    let file = fs::File::open(archive)?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| UpdateError::UnsafeArchive(e.to_string()))?;
    fs::create_dir_all(staging)?;
    let mut app = None;
    let mut updater = None;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|e| UpdateError::UnsafeArchive(e.to_string()))?;
        let name = entry.name().to_string();
        if entry.is_dir() {
            continue;
        }
        if entry.enclosed_name() != Some(Path::new(&name).to_path_buf())
            || name.contains('/')
            || name.contains('\\')
            || (name != "gocode.exe" && name != "gocode-updater.exe")
        {
            return Err(UpdateError::UnsafeArchive(format!(
                "unexpected archive entry: {name}"
            )));
        }
        let destination = staging.join(&name);
        let mut output = fs::File::create(&destination)?;
        io::copy(&mut entry, &mut output)?;
        if name == "gocode.exe" {
            app = Some(destination);
        } else {
            updater = Some(destination);
        }
    }
    match (app, updater) {
        (Some(app), Some(updater)) => Ok((app, updater)),
        _ => Err(UpdateError::UnsafeArchive(
            "archive must contain gocode.exe and gocode-updater.exe".into(),
        )),
    }
}

/// Replaces a stopped executable transactionally, restoring its backup if replacement fails.
///
/// # Errors
///
/// Returns an error when the source/target is invalid, replacement fails, or rollback fails.
pub fn replace_with_rollback(staged: &Path, installed: &Path) -> Result<(), UpdateError> {
    if !staged.is_file() || installed.file_name() != Some(std::ffi::OsStr::new("gocode.exe")) {
        return Err(UpdateError::Replace(
            "invalid update source or target".into(),
        ));
    }
    let backup = installed.with_extension("exe.previous");
    let _ = fs::remove_file(&backup);
    fs::rename(installed, &backup).map_err(|e| UpdateError::Replace(e.to_string()))?;
    if let Err(error) = fs::rename(staged, installed) {
        fs::rename(&backup, installed)
            .map_err(|rollback| UpdateError::Rollback(rollback.to_string()))?;
        return Err(UpdateError::Replace(error.to_string()));
    }
    let _ = fs::remove_file(backup);
    Ok(())
}

/// # Errors
///
/// Returns an error when the replacement executable cannot be started.
pub fn restart(installed: &Path, args: &[String]) -> Result<(), UpdateError> {
    Command::new(installed)
        .args(args)
        .spawn()
        .map(|_| ())
        .map_err(|e| UpdateError::Replace(format!("could not restart Gocode: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn asset(name: &str) -> ReleaseAsset {
        ReleaseAsset {
            name: name.into(),
            download_url: format!("https://example.test/{name}"),
        }
    }
    fn release(version: &str) -> ReleaseInfo {
        ReleaseInfo {
            version: Version::parse(version).unwrap(),
            notes: "notes".into(),
            assets: vec![
                asset(&format!("gocode-{version}-{WINDOWS_ARCHIVE_SUFFIX}")),
                asset(CHECKSUM_ASSET),
            ],
        }
    }

    #[test]
    fn a_newer_stable_release_is_offered() {
        assert_eq!(
            available_update(&Version::parse("0.1.0").unwrap(), [release("0.2.0")])
                .unwrap()
                .version,
            Version::parse("0.2.0").unwrap()
        );
    }
    #[test]
    fn same_or_older_release_is_not_offered() {
        assert!(
            available_update(
                &Version::parse("0.2.0").unwrap(),
                [release("0.2.0"), release("0.1.9")]
            )
            .is_none()
        );
    }
    #[test]
    fn missing_release_assets_are_not_offered() {
        let mut release = release("0.2.0");
        release.assets.pop();
        assert!(available_update(&Version::parse("0.1.0").unwrap(), [release]).is_none());
    }
    #[test]
    fn only_https_urls_are_accepted() {
        assert!(official_download_url("http://example.test/a").is_err());
        assert!(official_download_url("https://example.test/a").is_ok());
    }
    #[test]
    fn checksum_parser_requires_a_valid_matching_hash() {
        assert_eq!(
            checksum_for(&format!("{}  release.zip", "a".repeat(64)), "release.zip").unwrap(),
            "a".repeat(64)
        );
        assert!(checksum_for("bad release.zip", "release.zip").is_err());
    }
    #[test]
    fn checksum_mismatch_leaves_the_source_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("release.zip");
        fs::write(&file, "untrusted").unwrap();
        assert_eq!(
            verify_sha256(&file, &"0".repeat(64)),
            Err(UpdateError::ChecksumMismatch)
        );
        assert_eq!(fs::read_to_string(file).unwrap(), "untrusted");
    }
    #[test]
    fn replacement_swaps_the_binary_and_removes_the_backup() {
        let dir = tempfile::tempdir().unwrap();
        let installed = dir.path().join("gocode.exe");
        let staged = dir.path().join("new.exe");
        fs::write(&installed, "old").unwrap();
        fs::write(&staged, "new").unwrap();
        replace_with_rollback(&staged, &installed).unwrap();
        assert_eq!(fs::read_to_string(&installed).unwrap(), "new");
        assert!(!installed.with_extension("exe.previous").exists());
    }
    #[test]
    fn archive_rejects_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("release.zip");
        let file = fs::File::create(&archive).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file("../gocode.exe", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"bad").unwrap();
        writer.finish().unwrap();
        assert!(matches!(
            extract_windows_archive(&archive, &dir.path().join("staging")),
            Err(UpdateError::UnsafeArchive(_))
        ));
    }
}
