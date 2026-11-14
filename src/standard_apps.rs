//! Headless first-login Store client. It neither draws windows nor elevates
//! itself: the existing broker owns the device queue and authorizes each plan.
use mochios_package_protocol::provisioning as wire;
use std::time::Duration;

pub(super) fn retry_delay(attempts: u32) -> Duration {
    Duration::from_secs((30u64 << attempts.saturating_sub(1).min(6)).min(1800))
}

#[cfg(target_os = "mochios")]
mod native {
    use super::*;
    use crate::{
        DataRequest, PACKAGE_REQUEST_PREFIX, RELEASE_REQUEST_PREFIX, api, compatible_release,
        digest_matches, next_request_id,
    };
    use mochi_user_platform as p;
    use mochios_package_client::{self as packages, StandardApplicationProgress};
    use sha2::{Digest, Sha256};
    use std::{
        fs::{self, File, OpenOptions},
        io::Write,
        os::unix::fs::OpenOptionsExt,
        time::Instant,
    };

    fn native_error(error: p::syscall::SysError) -> String {
        format!("System operation failed (status {}).", error.raw())
    }

    fn control(request: wire::Request<'_>) -> Result<Vec<StandardApplicationProgress>, String> {
        packages::standard_applications(request).map_err(|error| error.to_string())
    }

    fn progress(index: usize, phase: wire::Phase, percent: u8, error: &str) -> Result<(), String> {
        control(wire::Request::Progress {
            index,
            phase,
            percent,
            error,
        })
        .map(|_| ())
    }

    fn wait<T>(
        request: u64,
        network: u64,
        start: DataRequest<T>,
        finish: impl Fn(&[u8]) -> Option<(u64, Result<T, api::ApiError>)>,
    ) -> Result<T, String> {
        if let DataRequest::Ready(value) = start {
            return Ok(value);
        }
        let deadline = Instant::now() + Duration::from_secs(130);
        let mut bytes = [0; 8192];
        loop {
            if Instant::now() >= deadline {
                return Err("The App Store request timed out.".into());
            }
            let mut objects = p::ipc::IpcObjectHandles::default();
            match p::ipc::try_wait_object_handles(&mut bytes, &mut objects) {
                Ok(message) => {
                    let has_objects = objects.count != 0 || !objects.is_user_delivery();
                    for object in objects
                        .handles
                        .iter()
                        .take((objects.count as usize).min(objects.handles.len()))
                    {
                        if object.handle != 0 {
                            let _ = p::handle::close(object.handle);
                        }
                    }
                    let length = message as u32 as usize;
                    if message >> 32 != network || length > bytes.len() || has_objects {
                        continue;
                    }
                    let bytes = &bytes[..length];
                    if api::response_request_id(bytes) != Some(request) {
                        api::discard_response(bytes);
                        continue;
                    }
                    return finish(bytes)
                        .ok_or_else(|| "Invalid App Store response.".to_owned())?
                        .1
                        .map_err(|error| error.to_string());
                }
                Err(error) if error.native_error() == Some(p::syscall::NativeError::NotReady) => {
                    p::thread::sleep_milliseconds(50).map_err(native_error)?;
                }
                Err(error) => return Err(native_error(error)),
            }
        }
    }

    struct Download {
        path: String,
        file: File,
    }

    impl Download {
        fn create() -> Result<Self, String> {
            let mut random = [0; 16];
            p::random::fill(&mut random).map_err(native_error)?;
            let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
            let path = format!("/tmp/appstore-setup-{suffix}.mpkg");
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .map_err(|error| error.to_string())?;
            Ok(Self { path, file })
        }
    }

    impl Drop for Download {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    fn install(index: usize) -> Result<(), String> {
        let bundle_id = wire::APPLICATIONS[index].0;
        progress(index, wire::Phase::Downloading, 0, "")?;
        let network = api::network_service().map_err(|e| e.to_string())?;
        let id = next_request_id(RELEASE_REQUEST_PREFIX);
        let response = wait(
            id,
            network,
            api::start_release_request(id, bundle_id).map_err(|e| e.to_string())?,
            api::finish_release_request,
        )?;
        let release = compatible_release(response, bundle_id)?
            .ok_or_else(|| "A compatible release has not been published yet.".to_owned())?;

        let mut download = Download::create()?;
        let mut hash = Sha256::new();
        let size = usize::try_from(release.size).map_err(|e| e.to_string())?;
        let mut offset = 0;
        let mut displayed_percent = 0;
        while offset < size {
            let current = control(wire::Request::Claim)?;
            if current[index].phase == wire::Phase::Complete {
                return Ok(());
            }
            let requested = api::MAX_PACKAGE_CHUNK_BYTES.min(size - offset);
            let id = next_request_id(PACKAGE_REQUEST_PREFIX);
            let bytes = wait(
                id,
                network,
                api::start_package_request(id, bundle_id, &release.version, offset, requested)
                    .map_err(|e| e.to_string())?,
                api::finish_package_request,
            )?;
            if bytes.len() != requested {
                return Err("The package download ended unexpectedly.".into());
            }
            download
                .file
                .write_all(&bytes)
                .map_err(|error| error.to_string())?;
            hash.update(&bytes);
            offset += bytes.len();
            let percent = (offset as u64 * 100 / size as u64) as u8;
            // Persist meaningful changes, not one fsync per network read.
            if percent >= displayed_percent + 5 || percent == 100 {
                progress(index, wire::Phase::Downloading, percent, "")?;
                displayed_percent = percent;
            }
        }
        download
            .file
            .sync_all()
            .map_err(|error| error.to_string())?;
        let digest = hash.finalize();
        if !digest_matches(&digest, &release.sha256) {
            return Err("Package SHA-256 verification failed.".into());
        }
        progress(index, wire::Phase::Installing, 100, "")?;
        packages::install_standard_application(
            &download.path,
            packages::Expected {
                package_id: bundle_id,
                version: &release.version,
                source_digest: digest.into(),
            },
        )
        .map_err(|error| error.to_string())?;

        // A successful RPC alone is insufficient: reconciliation must see the
        // activated package record before this application is considered done.
        let rows = control(wire::Request::Query)?;
        if rows[index].phase != wire::Phase::Complete {
            return Err("The installed package has not been activated.".into());
        }
        Ok(())
    }

    fn local_error(error: &str) -> String {
        let mut message = String::new();
        for c in error.chars().filter(|c| !c.is_control()) {
            if message.len() + c.len_utf8() > wire::MAX_ERROR {
                break;
            }
            message.push(c);
        }
        message
    }

    pub(super) fn run() -> Result<(), String> {
        let _ = p::logger::init_from_env();
        p::ipc::create().map_err(native_error)?;
        // Session publication and the broker's startup run independently.
        // Wait briefly for their authenticated route; never substitute a UID
        // check or launch an interactive elevation prompt as a fallback.
        let startup_deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match packages::standard_applications(wire::Request::Claim) {
                Ok(_) => break,
                Err(error) if error.raw_os_error() == Some(11) => return Ok(()),
                Err(error) if Instant::now() >= startup_deadline => return Err(error.to_string()),
                Err(_) => std::thread::sleep(Duration::from_secs(2)),
            }
        }
        let mut next_attempt = [Instant::now(); 5];
        loop {
            let rows = match control(wire::Request::Claim) {
                Ok(rows) => rows,
                // Another login already owns the worker. No parallel downloads.
                Err(error) => return Err(error),
            };
            if rows.iter().all(|row| row.phase == wire::Phase::Complete) {
                control(wire::Request::Release)?;
                return Ok(());
            }
            for index in 0..wire::APPLICATIONS.len() {
                if rows[index].phase == wire::Phase::Complete
                    || Instant::now() < next_attempt[index]
                {
                    continue;
                }
                if let Err(error) = install(index) {
                    let current = control(wire::Request::Query)?;
                    // A concurrent manual install can have satisfied the queue.
                    if current[index].phase != wire::Phase::Complete {
                        progress(index, wire::Phase::Failed, 0, &local_error(&error))?;
                        next_attempt[index] = Instant::now() + retry_delay(current[index].attempts);
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(15));
        }
    }
}

pub(super) fn run() -> Result<(), String> {
    #[cfg(target_os = "mochios")]
    {
        native::run()
    }
    #[cfg(not(target_os = "mochios"))]
    {
        Err("Automatic application setup requires mochiOS.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_back_off_without_overflow_or_busy_looping() {
        assert_eq!(retry_delay(0).as_secs(), 30);
        assert_eq!(retry_delay(1).as_secs(), 30);
        assert_eq!(retry_delay(2).as_secs(), 60);
        assert_eq!(retry_delay(u32::MAX).as_secs(), 1800);
    }

    #[test]
    #[cfg(not(target_os = "mochios"))]
    fn host_preview_cannot_install_or_synthesize_completion() {
        assert!(run().unwrap_err().contains("requires mochiOS"));
    }
}
