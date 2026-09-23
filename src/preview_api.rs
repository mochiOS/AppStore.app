use std::fmt;
use std::process::Command;

use crate::catalog::{PRODUCTION_API_BASE_URL, Storefront};

pub(crate) enum IconRequest {
    Pending,
    Ready(Vec<u8>),
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

pub(crate) fn start_icon_request(_request_id: u64, url: &str) -> Result<IconRequest, ApiError> {
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
    Ok(IconRequest::Ready(output.stdout))
}

pub(crate) fn finish_icon_request(_message: &[u8]) -> Option<(u64, Result<Vec<u8>, ApiError>)> {
    None
}
