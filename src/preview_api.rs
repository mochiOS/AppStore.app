use std::fmt;
use std::process::Command;

use crate::catalog::{PRODUCTION_API_BASE_URL, ReleaseResponse, Storefront};

pub(crate) const MAX_PACKAGE_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const MAX_PACKAGE_CHUNK_BYTES: usize = 2 * 1024 * 1024;

pub(crate) enum DataRequest<T> {
    Pending,
    Ready(T),
}

#[derive(Debug)]
pub(crate) enum ApiError {
    Request(String),
    InvalidJson(serde_json::Error),
}

impl fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request(message) => write!(formatter, "App Store API request failed: {message}"),
            Self::InvalidJson(error) => write!(formatter, "App Store API returned invalid JSON: {error}"),
        }
    }
}

pub(crate) fn fetch_storefront() -> Result<Storefront, ApiError> {
    let base_url = std::env::var("APPSTORE_API_BASE_URL")
        .unwrap_or_else(|_| PRODUCTION_API_BASE_URL.to_string());
    let url = format!("{}/storefront", base_url.trim_end_matches('/'));
    let output = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "15",
            "--max-filesize",
            "4194304",
            &url,
        ])
        .output()
        .map_err(|error| ApiError::Request(error.to_string()))?;
    if !output.status.success() {
        return Err(ApiError::Request(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(ApiError::InvalidJson)
}

pub(crate) fn start_icon_request(
    _request_id: u64,
    url: &str,
) -> Result<DataRequest<Vec<u8>>, ApiError> {
    if !url.starts_with("https://") {
        return Err(ApiError::Request(String::from("icon URL must use HTTPS")));
    }
    let output = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "5",
            "--max-filesize",
            "2097152",
            url,
        ])
        .output()
        .map_err(|error| ApiError::Request(error.to_string()))?;
    if !output.status.success() {
        return Err(ApiError::Request(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(DataRequest::Ready(output.stdout))
}

pub(crate) fn finish_icon_request(_message: &[u8]) -> Option<(u64, Result<Vec<u8>, ApiError>)> {
    None
}

pub(crate) fn start_release_request(
    _request_id: u64,
    bundle_id: &str,
) -> Result<DataRequest<ReleaseResponse>, ApiError> {
    let base_url = std::env::var("APPSTORE_API_BASE_URL")
        .unwrap_or_else(|_| PRODUCTION_API_BASE_URL.to_string());
    let url = format!(
        "{}/apps/{}/releases?architecture=x86_64&abi=mochios-1",
        base_url.trim_end_matches('/'),
        encode_component(bundle_id),
    );
    let bytes = curl(&url, "15", "1048576", false, None)?;
    serde_json::from_slice(&bytes)
        .map(DataRequest::Ready)
        .map_err(ApiError::InvalidJson)
}

pub(crate) fn finish_release_request(
    _message: &[u8],
) -> Option<(u64, Result<ReleaseResponse, ApiError>)> {
    None
}

pub(crate) fn start_package_request(
    _request_id: u64,
    bundle_id: &str,
    version: &str,
    offset: usize,
    length: usize,
) -> Result<DataRequest<Vec<u8>>, ApiError> {
    let base_url = std::env::var("APPSTORE_API_BASE_URL")
        .unwrap_or_else(|_| PRODUCTION_API_BASE_URL.to_string());
    let url = format!(
        "{}/apps/{}/download?version={}&architecture=x86_64&abi=mochios-1",
        base_url.trim_end_matches('/'),
        encode_component(bundle_id),
        encode_component(version),
    );
    let end = offset
        .checked_add(length)
        .and_then(|value| value.checked_sub(1))
        .ok_or_else(|| ApiError::Request(String::from("invalid package byte range")))?;
    curl(
        &url,
        "120",
        &(MAX_PACKAGE_CHUNK_BYTES + 1).to_string(),
        true,
        Some(&format!("{offset}-{end}")),
    )
    .map(DataRequest::Ready)
}

pub(crate) fn finish_package_request(
    _message: &[u8],
) -> Option<(u64, Result<Vec<u8>, ApiError>)> {
    None
}

pub(crate) fn response_request_id(_message: &[u8]) -> Option<u64> {
    None
}

fn curl(
    url: &str,
    timeout: &str,
    maximum: &str,
    follow: bool,
    range: Option<&str>,
) -> Result<Vec<u8>, ApiError> {
    let mut command = Command::new("curl");
    command.args([
        "--fail",
        "--silent",
        "--show-error",
        "--max-time",
        timeout,
        "--max-filesize",
        maximum,
    ]);
    if follow {
        command.arg("--location");
    }
    if let Some(range) = range {
        command.args(["--range", range]);
    }
    let output = command
        .arg(url)
        .output()
        .map_err(|error| ApiError::Request(error.to_string()))?;
    if !output.status.success() {
        return Err(ApiError::Request(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(output.stdout)
}

fn encode_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}
