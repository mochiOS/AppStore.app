use core::fmt;

use mochios_net_device_protocol::{
    HTTP_CLOSE_REQUEST_LEN, HTTP_READ_REQUEST_LEN, HTTP_READ_RESULT_BASE_LEN, HttpFailure,
    HttpMethod, HttpStream, MAX_HTTP_IPC_DATA_LEN, Opcode, decode_http_read_result,
    decode_http_request_result, encode_http_close, encode_http_read, encode_http_request,
};

use crate::catalog::{PRODUCTION_API_BASE_URL, ReleaseResponse, Storefront};

const REQUEST_TIMEOUT_MS: u32 = 15_000;
const ICON_REQUEST_TIMEOUT_MS: u32 = 5_000;
const PACKAGE_REQUEST_TIMEOUT_MS: u32 = 120_000;
const MAX_HEADERS_BYTES: usize = 16 * 1024;
const MAX_STOREFRONT_BYTES: usize = 4 * 1024 * 1024;
const MAX_ICON_BYTES: usize = 2 * 1024 * 1024;
const MAX_RELEASE_RESPONSE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_PACKAGE_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const MAX_PACKAGE_CHUNK_BYTES: usize = 2 * 1024 * 1024;

pub(crate) enum DataRequest<T> {
    Pending,
    Ready(T),
}

#[derive(Debug)]
pub(crate) enum ApiError {
    ServiceUnavailable(u64),
    Protocol,
    RequestIdMismatch,
    HandleMismatch,
    ServiceFailure { status: i32, failure: HttpFailure },
    ResponseTooLarge,
    Truncated,
    HttpStatus(u16),
    InvalidContentType,
    InvalidJson,
}

impl fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ServiceUnavailable(errno) => {
                write!(formatter, "network service unavailable (errno {errno})")
            }
            Self::Protocol => formatter.write_str("invalid network service response"),
            Self::RequestIdMismatch => formatter.write_str("network response did not match request"),
            Self::HandleMismatch => formatter.write_str("network response handle did not match"),
            Self::ServiceFailure { status, failure } => {
                write!(formatter, "network request failed ({failure:?}, status {status})")
            }
            Self::ResponseTooLarge => formatter.write_str("store response is too large"),
            Self::Truncated => formatter.write_str("store response ended unexpectedly"),
            Self::HttpStatus(status) => write!(formatter, "App Store API returned HTTP {status}"),
            Self::InvalidContentType => {
                formatter.write_str("App Store API returned an unsupported content type")
            }
            Self::InvalidJson => formatter.write_str("App Store API returned invalid catalog data"),
        }
    }
}

pub(crate) fn start_storefront_request(
    request_id: u64,
) -> Result<DataRequest<Storefront>, ApiError> {
    let base_url = option_env!("APPSTORE_API_BASE_URL").unwrap_or(PRODUCTION_API_BASE_URL);
    let url = format!("{}/storefront", base_url.trim_end_matches('/'));
    start_async_get(request_id, &url, REQUEST_TIMEOUT_MS, "").map(|_| DataRequest::Pending)
}

pub(crate) fn finish_storefront_request(
    message: &[u8],
) -> Option<(u64, Result<Storefront, ApiError>)> {
    finish_request(
        message,
        MAX_STOREFRONT_BYTES,
        |content_type| content_type.eq_ignore_ascii_case("application/json"),
        |status| status == 200,
    )
    .map(|(request_id, result)| {
        (
            request_id,
            result.and_then(|bytes| {
                serde_json::from_slice(&bytes).map_err(|_| ApiError::InvalidJson)
            }),
        )
    })
}

pub(crate) fn start_icon_request(
    request_id: u64,
    url: &str,
) -> Result<DataRequest<Vec<u8>>, ApiError> {
    if !url.starts_with("https://") {
        return Err(ApiError::Protocol);
    }
    start_async_get(request_id, url, ICON_REQUEST_TIMEOUT_MS, "")
}

pub(crate) fn start_release_request(
    request_id: u64,
    bundle_id: &str,
) -> Result<DataRequest<ReleaseResponse>, ApiError> {
    let base_url = option_env!("APPSTORE_API_BASE_URL").unwrap_or(PRODUCTION_API_BASE_URL);
    let url = format!(
        "{}/apps/{}/releases?architecture=x86_64&abi=mochios-1",
        base_url.trim_end_matches('/'),
        encode_component(bundle_id),
    );
    start_async_get(request_id, &url, REQUEST_TIMEOUT_MS, "").map(|_| DataRequest::Pending)
}

pub(crate) fn start_package_request(
    request_id: u64,
    bundle_id: &str,
    version: &str,
    offset: usize,
    length: usize,
) -> Result<DataRequest<Vec<u8>>, ApiError> {
    let base_url = option_env!("APPSTORE_API_BASE_URL").unwrap_or(PRODUCTION_API_BASE_URL);
    let url = format!(
        "{}/apps/{}/download?version={}&architecture=x86_64&abi=mochios-1",
        base_url.trim_end_matches('/'),
        encode_component(bundle_id),
        encode_component(version),
    );
    let end = offset
        .checked_add(length)
        .and_then(|value| value.checked_sub(1))
        .ok_or(ApiError::Protocol)?;
    let range = format!("bytes={offset}-{end}");
    start_async_get(request_id, &url, PACKAGE_REQUEST_TIMEOUT_MS, &range)
}

fn start_async_get(
    request_id: u64,
    url: &str,
    timeout_ms: u32,
    range: &str,
) -> Result<DataRequest<Vec<u8>>, ApiError> {
    let service = network_service()?;
    let mut request = vec![0; 48 + url.len()];
    let request_length = encode_http_request(
        request_id,
        HttpMethod::Get,
        timeout_ms,
        url,
        range,
        "",
        &[],
        &mut request,
    )
    .map_err(|_| ApiError::Protocol)?;
    mochi_user_platform::ipc::send(service, &request[..request_length])
        .map_err(|error| ApiError::ServiceUnavailable(error.errno().unwrap_or(0)))?;
    Ok(DataRequest::Pending)
}

pub(crate) fn finish_icon_request(message: &[u8]) -> Option<(u64, Result<Vec<u8>, ApiError>)> {
    finish_request(message, MAX_ICON_BYTES, |content_type| {
        ["image/png", "image/jpeg", "image/webp"]
            .iter()
            .any(|supported| content_type.eq_ignore_ascii_case(supported))
    }, |status| status == 200)
}

pub(crate) fn finish_release_request(
    message: &[u8],
) -> Option<(u64, Result<ReleaseResponse, ApiError>)> {
    finish_request(
        message,
        MAX_RELEASE_RESPONSE_BYTES,
        |content_type| content_type.eq_ignore_ascii_case("application/json"),
        |status| status == 200,
    )
    .map(|(request_id, result)| {
        (
            request_id,
            result.and_then(|bytes| {
                serde_json::from_slice(&bytes).map_err(|_| ApiError::InvalidJson)
            }),
        )
    })
}

pub(crate) fn finish_package_request(
    message: &[u8],
) -> Option<(u64, Result<Vec<u8>, ApiError>)> {
    finish_request(
        message,
        MAX_PACKAGE_CHUNK_BYTES,
        |content_type| {
            content_type.eq_ignore_ascii_case("application/octet-stream")
                || content_type.eq_ignore_ascii_case("application/x-mpkg")
        },
        |status| matches!(status, 200 | 206),
    )
}

pub(crate) fn response_request_id(message: &[u8]) -> Option<u64> {
    decode_http_request_result(message)
        .ok()
        .map(|result| result.request_id)
}

fn finish_request(
    message: &[u8],
    maximum_body_bytes: usize,
    valid_content_type: impl FnOnce(&str) -> bool,
    valid_status: impl FnOnce(u16) -> bool,
) -> Option<(u64, Result<Vec<u8>, ApiError>)> {
    let result = decode_http_request_result(message).ok()?;
    let request_id = result.request_id;
    let completed = (|| {
        if result.status != 0 || result.failure != HttpFailure::None {
            return Err(ApiError::ServiceFailure {
                status: result.status,
                failure: result.failure,
            });
        }
        if !valid_status(result.status_code) {
            let _ = close(request_id, result.handle);
            return Err(ApiError::HttpStatus(result.status_code));
        }
        let content_type = result
            .content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        if !valid_content_type(content_type) {
            let _ = close(request_id, result.handle);
            return Err(ApiError::InvalidContentType);
        }
        let body_length = result.body_length as usize;
        let headers_length = result.headers_length as usize;
        if body_length == 0
            || body_length > maximum_body_bytes
            || headers_length > MAX_HEADERS_BYTES
        {
            let _ = close(request_id, result.handle);
            return Err(ApiError::ResponseTooLarge);
        }
        let fetched = (|| {
            read_stream(request_id, result.handle, HttpStream::Headers, headers_length)?;
            read_stream(request_id, result.handle, HttpStream::Body, body_length)
        })();
        let closed = close(request_id, result.handle);
        match (fetched, closed) {
            (Ok(body), Ok(())) => Ok(body),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    })();
    Some((request_id, completed))
}

fn encode_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use core::fmt::Write;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn read_stream(
    request_id: u64,
    handle: u64,
    stream: HttpStream,
    expected_length: usize,
) -> Result<Vec<u8>, ApiError> {
    if expected_length == 0 {
        return Ok(Vec::new());
    }

    let mut output = Vec::with_capacity(expected_length);
    let mut complete = false;
    while output.len() < expected_length && !complete {
        let maximum = (expected_length - output.len()).min(MAX_HTTP_IPC_DATA_LEN);
        let mut request = [0; HTTP_READ_REQUEST_LEN];
        encode_http_read(request_id, handle, maximum as u32, stream, &mut request)
            .map_err(|_| ApiError::Protocol)?;
        let mut reply = vec![0; HTTP_READ_RESULT_BASE_LEN + maximum];
        let reply_length = call(&request, &mut reply)?;
        let (reply_id, status, failure, reply_handle, reply_complete, data) =
            decode_http_read_result(
                Opcode::HttpReadResult,
                reply.get(..reply_length).ok_or(ApiError::Protocol)?,
            )
            .map_err(|_| ApiError::Protocol)?;

        if reply_id != request_id {
            return Err(ApiError::RequestIdMismatch);
        }
        if reply_handle != handle {
            return Err(ApiError::HandleMismatch);
        }
        if status != 0 || failure != HttpFailure::None {
            return Err(ApiError::ServiceFailure { status, failure });
        }
        if data.is_empty() && !reply_complete {
            return Err(ApiError::Truncated);
        }
        output.extend_from_slice(data);
        complete = reply_complete;
    }

    if output.len() != expected_length || !complete {
        return Err(ApiError::Truncated);
    }
    Ok(output)
}

fn close(request_id: u64, handle: u64) -> Result<(), ApiError> {
    let mut request = [0; HTTP_CLOSE_REQUEST_LEN];
    encode_http_close(request_id, handle, &mut request).map_err(|_| ApiError::Protocol)?;
    let mut reply = [0; HTTP_READ_RESULT_BASE_LEN];
    let reply_length = call(&request, &mut reply)?;
    let (reply_id, status, failure, reply_handle, complete, data) = decode_http_read_result(
        Opcode::HttpCloseResult,
        reply.get(..reply_length).ok_or(ApiError::Protocol)?,
    )
    .map_err(|_| ApiError::Protocol)?;

    if reply_id != request_id {
        return Err(ApiError::RequestIdMismatch);
    }
    if reply_handle != handle || !complete || !data.is_empty() {
        return Err(ApiError::HandleMismatch);
    }
    if status != 0 || failure != HttpFailure::None {
        return Err(ApiError::ServiceFailure { status, failure });
    }
    Ok(())
}

fn call(request: &[u8], reply: &mut [u8]) -> Result<usize, ApiError> {
    let service = network_service()?;
    let result = mochi_user_platform::ipc::call(service, request, reply)
        .map_err(|error| ApiError::ServiceUnavailable(error.errno().unwrap_or(0)))?;
    let length = (result & 0xffff_ffff) as usize;
    if length > reply.len() {
        Err(ApiError::Protocol)
    } else {
        Ok(length)
    }
}

fn network_service() -> Result<u64, ApiError> {
    let service = mochi_user_platform::process::find_by_name("network.service")
        .map_err(|error| ApiError::ServiceUnavailable(error.errno().unwrap_or(0)))?;
    if service == 0 {
        Err(ApiError::ServiceUnavailable(
            mochi_user_platform::syscall::ENOENT,
        ))
    } else {
        Ok(service)
    }
}
