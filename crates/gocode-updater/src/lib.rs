//! Safe, user-approved release discovery and replacement primitives for Gocode.

use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::Command,
};

use futures_util::StreamExt;
use reqwest::Url;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const RELEASES_URL: &str = "https://api.github.com/repos/luizGTogni/gocode/releases";
pub const WINDOWS_ARCHIVE_SUFFIX: &str = "windows-x86_64.zip";
pub const LINUX_ARCHIVE_SUFFIX: &str = "linux-x86_64.tar.gz";
pub const CHECKSUM_ASSET: &str = "SHA256SUMS";

/// This platform's release archive suffix, or [`UpdateError::UnsupportedPlatform`] on anything
/// other than Windows or Linux.
///
/// # Errors
///
/// Returns [`UpdateError::UnsupportedPlatform`] on unsupported platforms.
pub fn current_platform_archive_suffix() -> Result<&'static str, UpdateError> {
    if cfg!(windows) {
        Ok(WINDOWS_ARCHIVE_SUFFIX)
    } else if cfg!(target_os = "linux") {
        Ok(LINUX_ARCHIVE_SUFFIX)
    } else {
        Err(UpdateError::UnsupportedPlatform)
    }
}

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
                f.write_str("The release does not contain the required update files.")
            }
            Self::ChecksumMissing => {
                f.write_str("The release does not provide a checksum for the update archive.")
            }
            Self::ChecksumMismatch => f.write_str("Gocode could not verify the downloaded update."),
            Self::UnsupportedPlatform => {
                f.write_str("Automatic updates are currently available only on Windows and Linux.")
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

/// Parses release metadata from GitHub and selects only a newer, stable release that ships an
/// archive matching `archive_suffix` (see [`WINDOWS_ARCHIVE_SUFFIX`], [`LINUX_ARCHIVE_SUFFIX`],
/// or [`current_platform_archive_suffix`]).
#[must_use]
pub fn available_update(
    current: &Version,
    releases: impl IntoIterator<Item = ReleaseInfo>,
    archive_suffix: &str,
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
                .find(|asset| asset.name == format!("{prefix}{archive_suffix}"))
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
        let endpoint = official_download_url(endpoint)?;
        Ok(Self {
            client: reqwest::Client::builder()
                .user_agent("gocode-updater")
                .build()
                .map_err(|e| UpdateError::Network(e.to_string()))?,
            endpoint,
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

/// Stores a fully downloaded artifact only after the partial file has been completed, reporting
/// `(downloaded_bytes, total_bytes)` to `on_progress` as each chunk arrives. `total_bytes` is
/// `None` when the server does not report a `Content-Length`.
///
/// # Errors
///
/// Returns an error for invalid URLs, failed requests, or staging I/O failures.
pub async fn download_to_staging(
    client: &reqwest::Client,
    url: &str,
    staging: &Path,
    mut on_progress: impl FnMut(u64, Option<u64>),
) -> Result<PathBuf, UpdateError> {
    let url = official_download_url(url)?;
    fs::create_dir_all(staging)?;
    let partial = staging.join("download.partial");
    let final_path = staging.join("release.download");
    let result = async {
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|e| UpdateError::Network(e.to_string()))?
            .error_for_status()
            .map_err(|e| UpdateError::Network(e.to_string()))?;
        let total = response.content_length();
        let mut file = tokio::fs::File::create(&partial)
            .await
            .map_err(|e| UpdateError::Io(e.to_string()))?;
        let mut downloaded: u64 = 0;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| UpdateError::Network(e.to_string()))?;
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
                .await
                .map_err(|e| UpdateError::Io(e.to_string()))?;
            downloaded += chunk.len() as u64;
            on_progress(downloaded, total);
        }
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

/// Extracts `gocode.exe` and `gocode-updater.exe` from a root-level `.zip` release archive.
/// Every entry must be a root-level file; other root-level files (`LICENSE`, `INSTALL.md`,
/// `install-windows.ps1`, …) are skipped, and anything nested in a subdirectory or resolving
/// outside the archive root (e.g. a path-traversal entry) is rejected as unsafe.
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
        {
            return Err(UpdateError::UnsafeArchive(format!(
                "unexpected archive entry: {name}"
            )));
        }
        if name != "gocode.exe" && name != "gocode-updater.exe" {
            // Root-level extras such as LICENSE, INSTALL.md, and install-windows.ps1 matter only
            // for a fresh manual install; the updater just skips them (see docs/UPDATER.md §65).
            continue;
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

/// Extracts the `gocode` executable from a `.tar.gz` release archive. Every entry must live
/// directly under a single top-level directory; the `gocode` file there is extracted, other
/// files there (`LICENSE`, install scripts, …) are skipped, and anything outside that one
/// top-level directory (e.g. a path-traversal entry) is rejected as unsafe.
///
/// # Errors
///
/// Returns an error for malformed archives, unsafe entries, a missing executable, or I/O failure.
pub fn extract_linux_archive(archive: &Path, staging: &Path) -> Result<PathBuf, UpdateError> {
    fs::create_dir_all(staging)?;
    let file = fs::File::open(archive)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);
    let mut app = None;
    for entry in tar
        .entries()
        .map_err(|e| UpdateError::UnsafeArchive(e.to_string()))?
    {
        let mut entry = entry.map_err(|e| UpdateError::UnsafeArchive(e.to_string()))?;
        if entry.header().entry_type().is_dir() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|e| UpdateError::UnsafeArchive(e.to_string()))?
            .into_owned();
        let mut components = path.components();
        let (
            Some(std::path::Component::Normal(_top_level)),
            Some(std::path::Component::Normal(name)),
            None,
        ) = (components.next(), components.next(), components.next())
        else {
            return Err(UpdateError::UnsafeArchive(format!(
                "unexpected archive entry: {}",
                path.display()
            )));
        };
        if name != "gocode" {
            continue;
        }
        let destination = staging.join("gocode");
        entry
            .unpack(&destination)
            .map_err(|e| UpdateError::UnsafeArchive(e.to_string()))?;
        app = Some(destination);
    }
    let app = app.ok_or_else(|| {
        UpdateError::UnsafeArchive("archive must contain a gocode executable".into())
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&app)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&app, permissions)?;
    }
    Ok(app)
}

/// Replaces a stopped (Windows) or running (Linux, via atomic rename) executable transactionally,
/// restoring its backup if replacement fails.
///
/// # Errors
///
/// Returns an error when the source is invalid, replacement fails, or rollback fails.
pub fn replace_with_rollback(staged: &Path, installed: &Path) -> Result<(), UpdateError> {
    if !staged.is_file() {
        return Err(UpdateError::Replace(
            "invalid update source or target".into(),
        ));
    }
    let backup = installed.with_extension("previous");
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
///
/// On Unix this replaces the current process image (`exec`) instead of spawning a child and
/// exiting, so the restarted binary keeps the same PID, session, and controlling terminal. A
/// spawn-then-exit handoff hands the child a fresh fd table right as the parent exits, which
/// can race the terminal driver and surface as an I/O error when the new process re-initializes
/// the terminal.
pub fn restart(installed: &Path, args: &[String]) -> Result<(), UpdateError> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = Command::new(installed).args(args).exec();
        Err(UpdateError::Replace(format!(
            "could not restart Gocode: {error}"
        )))
    }
    #[cfg(not(unix))]
    {
        Command::new(installed)
            .args(args)
            .spawn()
            .map(|_| ())
            .map_err(|e| UpdateError::Replace(format!("could not restart Gocode: {e}")))
    }
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
            available_update(
                &Version::parse("0.1.0").unwrap(),
                [release("0.2.0")],
                WINDOWS_ARCHIVE_SUFFIX
            )
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
                [release("0.2.0"), release("0.1.9")],
                WINDOWS_ARCHIVE_SUFFIX
            )
            .is_none()
        );
    }
    #[test]
    fn missing_release_assets_are_not_offered() {
        let mut release = release("0.2.0");
        release.assets.pop();
        assert!(
            available_update(
                &Version::parse("0.1.0").unwrap(),
                [release],
                WINDOWS_ARCHIVE_SUFFIX
            )
            .is_none()
        );
    }
    #[test]
    fn a_release_without_a_linux_archive_is_not_offered_on_linux() {
        let release = release("0.2.0");
        assert!(
            available_update(
                &Version::parse("0.1.0").unwrap(),
                [release],
                LINUX_ARCHIVE_SUFFIX
            )
            .is_none()
        );
    }
    #[test]
    fn a_release_with_a_linux_archive_is_offered_on_linux() {
        let version = "0.2.0";
        let release = ReleaseInfo {
            version: Version::parse(version).unwrap(),
            notes: "notes".into(),
            assets: vec![
                asset(&format!("gocode-{version}-{LINUX_ARCHIVE_SUFFIX}")),
                asset(CHECKSUM_ASSET),
            ],
        };
        assert_eq!(
            available_update(
                &Version::parse("0.1.0").unwrap(),
                [release],
                LINUX_ARCHIVE_SUFFIX
            )
            .unwrap()
            .version,
            Version::parse(version).unwrap()
        );
    }
    #[test]
    fn only_https_urls_are_accepted() {
        assert!(official_download_url("http://example.test/a").is_err());
        assert!(official_download_url("https://example.test/a").is_ok());
    }
    #[test]
    fn release_source_requires_an_https_endpoint() {
        assert!(GitHubReleaseSource::with_endpoint("http://example.test/releases").is_err());
        assert!(GitHubReleaseSource::with_endpoint("https://example.test/releases").is_ok());
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
        assert!(!installed.with_extension("previous").exists());
    }
    #[test]
    fn replacement_works_on_an_extensionless_linux_binary_name() {
        let dir = tempfile::tempdir().unwrap();
        let installed = dir.path().join("gocode");
        let staged = dir.path().join("gocode.new");
        fs::write(&installed, "old").unwrap();
        fs::write(&staged, "new").unwrap();
        replace_with_rollback(&staged, &installed).unwrap();
        assert_eq!(fs::read_to_string(&installed).unwrap(), "new");
        assert!(!installed.with_extension("previous").exists());
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
    #[test]
    fn windows_archive_skips_non_essential_root_level_files() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("release.zip");
        let file = fs::File::create(&archive).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        for (name, contents) in [
            ("gocode.exe", "app".as_bytes()),
            ("gocode-updater.exe", "updater".as_bytes()),
            ("LICENSE", "license".as_bytes()),
            ("INSTALL.md", "install".as_bytes()),
            ("install-windows.ps1", "script".as_bytes()),
        ] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(contents).unwrap();
        }
        writer.finish().unwrap();

        let staging = dir.path().join("staging");
        let (app, updater) = extract_windows_archive(&archive, &staging).unwrap();
        assert_eq!(fs::read_to_string(app).unwrap(), "app");
        assert_eq!(fs::read_to_string(updater).unwrap(), "updater");
        assert!(!staging.join("LICENSE").exists());
        assert!(!staging.join("install-windows.ps1").exists());
    }
    #[test]
    fn linux_archive_extracts_only_the_gocode_binary() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("release.tar.gz");
        let encoder = flate2::write::GzEncoder::new(
            fs::File::create(&archive).unwrap(),
            flate2::Compression::fast(),
        );
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(11);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(
                &mut header,
                "gocode-0.2.0-linux-x86_64/gocode",
                "new binary!".as_bytes(),
            )
            .unwrap();
        let mut license_header = tar::Header::new_gnu();
        license_header.set_size(7);
        license_header.set_mode(0o644);
        license_header.set_cksum();
        builder
            .append_data(
                &mut license_header,
                "gocode-0.2.0-linux-x86_64/LICENSE",
                "license".as_bytes(),
            )
            .unwrap();
        builder.into_inner().unwrap().finish().unwrap();

        let staging = dir.path().join("staging");
        let extracted = extract_linux_archive(&archive, &staging).unwrap();
        assert_eq!(fs::read_to_string(extracted).unwrap(), "new binary!");
    }
    #[test]
    fn linux_archive_rejects_entries_outside_a_single_top_level_directory() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("release.tar.gz");
        let encoder = flate2::write::GzEncoder::new(
            fs::File::create(&archive).unwrap(),
            flate2::Compression::fast(),
        );
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(3);
        header.set_mode(0o755);
        header.set_cksum();
        // No wrapping top-level directory, unlike every real release archive.
        builder
            .append_data(&mut header, "gocode", "bad".as_bytes())
            .unwrap();
        builder.into_inner().unwrap().finish().unwrap();

        assert!(matches!(
            extract_linux_archive(&archive, &dir.path().join("staging")),
            Err(UpdateError::UnsafeArchive(_))
        ));
    }
}
