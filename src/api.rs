use core::fmt;

use mochios_net_device_protocol::{
    HTTP_CLOSE_REQUEST_LEN, HTTP_READ_REQUEST_LEN, HTTP_READ_RESULT_BASE_LEN,
    HTTP_REQUEST_RESULT_BASE_LEN, HttpFailure, HttpMethod, HttpStream, MAX_HTTP_CONTENT_TYPE_LEN,
    MAX_HTTP_IPC_DATA_LEN, Opcode, decode_http_read_result, decode_http_request_result,
    encode_http_close, encode_http_read, encode_http_request,
};

use crate::catalog::Storefront;

const PRODUCTION_API_BASE_URL: &str = "https://api.store.mochios.org/v1";
const REQUEST_TIMEOUT_MS: u32 = 15_000;
const MAX_HEADERS_BYTES: usize = 16 * 1024;
const MAX_STOREFRONT_BYTES: usize = 4 * 1024 * 1024;

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

pub(crate) fn fetch_storefront() -> Result<Storefront, ApiError> {
    let base_url = option_env!("APPSTORE_API_BASE_URL").unwrap_or(PRODUCTION_API_BASE_URL);
    let url = format!("{}/storefront", base_url.trim_end_matches('/'));
    let request_id = mochi_user_platform::time::ticks().unwrap_or(1).max(1);
    let response = get(request_id, &url)?;

    if response.status_code != 200 {
        return Err(ApiError::HttpStatus(response.status_code));
    }
    if !response
        .content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("application/json")
    {
        return Err(ApiError::InvalidContentType);
    }

    serde_json::from_slice(&response.body).map_err(|_| ApiError::InvalidJson)
}

struct Response {
    status_code: u16,
    content_type: String,
    body: Vec<u8>,
}

fn get(request_id: u64, url: &str) -> Result<Response, ApiError> {
    let mut request = vec![0; 48 + url.len()];
    let request_length = encode_http_request(
        request_id,
        HttpMethod::Get,
        REQUEST_TIMEOUT_MS,
        url,
        "",
        "",
        &[],
        &mut request,
    )
    .map_err(|_| ApiError::Protocol)?;
    let mut reply = [0; HTTP_REQUEST_RESULT_BASE_LEN + MAX_HTTP_CONTENT_TYPE_LEN];
    let reply_length = call(&request[..request_length], &mut reply)?;
    let result = decode_http_request_result(reply.get(..reply_length).ok_or(ApiError::Protocol)?)
        .map_err(|_| ApiError::Protocol)?;

    if result.request_id != request_id {
        return Err(ApiError::RequestIdMismatch);
    }
    if result.status != 0 || result.failure != HttpFailure::None {
        return Err(ApiError::ServiceFailure {
            status: result.status,
            failure: result.failure,
        });
    }

    let handle = result.handle;
    let fetched = (|| {
        let body_length = result.body_length as usize;
        let headers_length = result.headers_length as usize;
        if body_length > MAX_STOREFRONT_BYTES || headers_length > MAX_HEADERS_BYTES {
            return Err(ApiError::ResponseTooLarge);
        }

        read_stream(request_id, handle, HttpStream::Headers, headers_length)?;
        let body = read_stream(request_id, handle, HttpStream::Body, body_length)?;
        Ok(Response {
            status_code: result.status_code,
            content_type: result.content_type.to_string(),
            body,
        })
    })();
    let closed = close(request_id, handle);

    match (fetched, closed) {
        (Ok(response), Ok(())) => Ok(response),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
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
    let service = mochi_user_platform::process::find_by_name("network.service")
        .map_err(|error| ApiError::ServiceUnavailable(error.errno().unwrap_or(0)))?;
    if service == 0 {
        return Err(ApiError::ServiceUnavailable(
            mochi_user_platform::syscall::ENOENT,
        ));
    }
    let result = mochi_user_platform::ipc::call(service, request, reply)
        .map_err(|error| ApiError::ServiceUnavailable(error.errno().unwrap_or(0)))?;
    let length = (result & 0xffff_ffff) as usize;
    if length > reply.len() {
        Err(ApiError::Protocol)
    } else {
        Ok(length)
    }
}
