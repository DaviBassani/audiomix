//! Native Windows Core Audio (WASAPI) mirroring core.
//!
//! This module is the Windows counterpart of the Linux `pactl` backend. It
//! enumerates active render endpoints through Core Audio, captures the current
//! default render endpoint with WASAPI loopback, and renders that capture to
//! one or more explicitly selected secondary render endpoints.
//!
//! Deliberate limitations of this first Windows version:
//!
//! * No A2DP/profile selection, no global default-device control, and no
//!   automatic default changes. The caller selects the current default output
//!   as the source; if it is not the current default, startup fails.
//! * The source endpoint is never used as a render target, so the captured
//!   audio cannot be fed back into itself.
//! * Audio is streamed in shared mode. Format, channel and sample-rate
//!   conversion is delegated to the Windows audio engine by initialising every
//!   secondary render stream with the source mix format and
//!   `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY`.
//!   The source mix format itself is validated first; unsupported layouts are
//!   rejected before any stream is started.
//!
//! All public entry points are usable from any thread. Windows calls initialise
//! COM on the thread that performs them and release it with RAII. The mirror
//! worker owns all Core Audio objects on one COM-initialised thread, so audio
//! interfaces never cross apartments.
//!
//! Nothing in this module prints to stdout/stderr. Errors are returned as
//! [`AudioError`], runtime health through [`MirrorStats`], and lifecycle
//! transitions through the optional [`MirrorEvent`] channel.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::AtomicBool;
#[cfg(windows)]
use std::sync::atomic::Ordering;
use std::sync::Arc;
#[cfg(windows)]
use std::sync::Mutex;

/// A native active render endpoint.
///
/// `id` is the stable Core Audio endpoint ID returned by `IMMDevice::GetId`.
/// `name` is the friendly endpoint name. `channels` and `sample_rate` describe
/// the endpoint's current device format when Windows exposes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub channels: Option<u16>,
    pub sample_rate: Option<u32>,
}

/// The validated source/target pair used to start a mirror session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorPlan {
    pub source: Endpoint,
    pub targets: Vec<Endpoint>,
}

/// Sample representation of a validated PCM format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcmSampleType {
    Float,
    Int,
}

/// A single validated uncompressed format description.
///
/// This is a pure, platform-independent description. On Windows it is built
/// from the source endpoint's `GetMixFormat` result. `stored_bits` is the
/// container width; `valid_bits` is the meaningful width (for example 24 valid
/// bits in a 32-bit container).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmFormat {
    pub sample_type: PcmSampleType,
    pub stored_bits: u16,
    pub valid_bits: u16,
    pub channels: u16,
    pub sample_rate: u32,
    pub block_align: u16,
}

impl PcmFormat {
    pub fn bytes_per_frame(&self) -> usize {
        self.block_align as usize
    }
}

/// Classification for actionable failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioErrorKind {
    UnsupportedPlatform,
    ComInitialization,
    Enumeration,
    DuplicateEndpoint,
    TooFewOutputs,
    SourceMissing,
    SourceNotDefault,
    NoTargets,
    DuplicateTarget,
    TargetIsSource,
    TargetMissing,
    UnsupportedFormat,
    Initialization,
    DeviceInvalidated,
    Timeout,
    Runtime,
}

/// An error with a machine-readable kind and a user-facing message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioError {
    kind: AudioErrorKind,
    message: String,
}

impl AudioError {
    pub fn new(kind: AudioErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> AudioErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn unsupported_platform() -> Self {
        Self::new(
            AudioErrorKind::UnsupportedPlatform,
            "native WASAPI audio is only available on Windows",
        )
    }
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AudioError {}

/// Lifecycle/health events delivered on the optional status channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirrorEvent {
    Started {
        source: Endpoint,
        targets: Vec<Endpoint>,
    },
    Fatal {
        error: AudioError,
    },
    Stopped,
}

/// Per-target runtime health.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetHealth {
    pub id: String,
    pub name: String,
    pub frames_rendered: u64,
    pub overflow_frames: u64,
    pub underrun_frames: u64,
    pub last_error: Option<String>,
}

/// Snapshot of mirror-session health. Reading this never blocks audio for
/// longer than a short mutex acquisition.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MirrorStats {
    pub running: bool,
    pub frames_captured: u64,
    pub frames_rendered: u64,
    pub overflow_frames: u64,
    pub underrun_frames: u64,
    pub last_error: Option<String>,
    pub targets: Vec<TargetHealth>,
}

/// Validates a pure endpoint selection before any Core Audio object is opened.
///
/// Rules enforced here:
/// * at least two active render endpoints exist;
/// * the requested source exists and is the current default render endpoint;
/// * at least one target is supplied;
/// * targets are unique, exist, and do not include the source.
pub fn select_mirror_plan(
    endpoints: &[Endpoint],
    default_id: Option<&str>,
    source_id: &str,
    target_ids: &[String],
) -> Result<MirrorPlan, AudioError> {
    if endpoints.len() < 2 {
        return Err(AudioError::new(
            AudioErrorKind::TooFewOutputs,
            format!(
                "mirroring needs at least two active render outputs, found {}",
                endpoints.len()
            ),
        ));
    }

    let mut ids = endpoints.iter().map(|e| e.id.as_str()).collect::<Vec<_>>();
    ids.sort_unstable();
    if let Some(pair) = ids.windows(2).find(|w| w[0] == w[1]) {
        return Err(AudioError::new(
            AudioErrorKind::DuplicateEndpoint,
            format!("Core Audio returned duplicate endpoint id {:?}", pair[0]),
        ));
    }

    let source = endpoints
        .iter()
        .find(|e| e.id == source_id)
        .cloned()
        .ok_or_else(|| {
            AudioError::new(
                AudioErrorKind::SourceMissing,
                format!(
                    "source render endpoint {:?} is not an active output",
                    source_id
                ),
            )
        })?;

    match default_id {
        Some(default) if default == source_id => {}
        Some(default) => {
            return Err(AudioError::new(
                AudioErrorKind::SourceNotDefault,
                format!(
                    "source {:?} is not the current default render output ({:?}); select the default output as source",
                    source_id, default
                ),
            ));
        }
        None => {
            return Err(AudioError::new(
                AudioErrorKind::SourceNotDefault,
                "Windows reports no default render output; select a default output before starting",
            ));
        }
    }

    if target_ids.is_empty() {
        return Err(AudioError::new(
            AudioErrorKind::NoTargets,
            "select at least one secondary render output",
        ));
    }

    let mut seen = Vec::with_capacity(target_ids.len());
    let mut targets = Vec::with_capacity(target_ids.len());
    for target_id in target_ids {
        if target_id == source_id {
            return Err(AudioError::new(
                AudioErrorKind::TargetIsSource,
                format!(
                    "target {:?} is the source; refusing to render captured audio back to the source",
                    target_id
                ),
            ));
        }
        if seen.contains(&target_id) {
            return Err(AudioError::new(
                AudioErrorKind::DuplicateTarget,
                format!("target {:?} was selected more than once", target_id),
            ));
        }
        seen.push(target_id);
        let endpoint = endpoints
            .iter()
            .find(|e| &e.id == target_id)
            .cloned()
            .ok_or_else(|| {
                AudioError::new(
                    AudioErrorKind::TargetMissing,
                    format!(
                        "target render endpoint {:?} is missing or inactive",
                        target_id
                    ),
                )
            })?;
        targets.push(endpoint);
    }

    Ok(MirrorPlan { source, targets })
}

const MIN_SAMPLE_RATE: u32 = 8_000;
const MAX_SAMPLE_RATE: u32 = 384_000;
const MAX_CHANNELS: u16 = 32;

/// Validates an uncompressed PCM/float format without guessing.
///
/// Accepted layouts:
/// * IEEE float 32- or 64-bit, all stored bits valid;
/// * integer PCM 8, 16, 24 or 32-bit containers with all bits valid, plus the
///   common 24-valid-in-32 case.
///
/// `block_align` must equal `channels * ceil(stored_bits / 8)`. Anything else
/// is rejected instead of being reinterpreted.
pub fn validate_pcm_format(format: &PcmFormat) -> Result<(), AudioError> {
    if format.channels == 0 || format.channels > MAX_CHANNELS {
        return Err(AudioError::new(
            AudioErrorKind::UnsupportedFormat,
            format!(
                "unsupported channel count {} (supported: 1..={})",
                format.channels, MAX_CHANNELS
            ),
        ));
    }
    if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&format.sample_rate) {
        return Err(AudioError::new(
            AudioErrorKind::UnsupportedFormat,
            format!(
                "unsupported sample rate {} Hz (supported: {}..={} Hz)",
                format.sample_rate, MIN_SAMPLE_RATE, MAX_SAMPLE_RATE
            ),
        ));
    }

    let expected_block_align = format
        .channels
        .checked_mul(format.stored_bits.div_ceil(8))
        .ok_or_else(|| {
            AudioError::new(
                AudioErrorKind::UnsupportedFormat,
                "format block alignment overflows",
            )
        })?;
    if format.block_align == 0 || format.block_align != expected_block_align {
        return Err(AudioError::new(
            AudioErrorKind::UnsupportedFormat,
            format!(
                "block alignment {} does not match {} channel(s) x {} stored bits ({})",
                format.block_align, format.channels, format.stored_bits, expected_block_align
            ),
        ));
    }

    let bits_ok = match format.sample_type {
        PcmSampleType::Float => {
            matches!(format.stored_bits, 32 | 64) && format.valid_bits == format.stored_bits
        }
        PcmSampleType::Int => match format.stored_bits {
            8 | 16 | 24 | 32 => {
                format.valid_bits == format.stored_bits
                    || (format.stored_bits == 32 && format.valid_bits == 24)
            }
            _ => false,
        },
    };
    if !bits_ok {
        return Err(AudioError::new(
            AudioErrorKind::UnsupportedFormat,
            format!(
                "unsupported sample layout: {:?} {} stored bits / {} valid bits",
                format.sample_type, format.stored_bits, format.valid_bits
            ),
        ));
    }

    Ok(())
}

/// Result of appending a packet to a bounded per-sink staging queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundedPush {
    pub dropped_frames: u64,
}

/// Appends whole frames to a bounded byte queue.
///
/// The queue is kept as a multiple of `block_align`. When the queue would
/// exceed `max_bytes`, the oldest whole frames are dropped first, then excess
/// input frames. This is explicit overflow behaviour: callers record the
/// returned `dropped_frames`. Capture-only flags must not be passed to render
/// clients when reporting these drops.
pub fn push_bounded_frames(
    queue: &mut VecDeque<u8>,
    data: &[u8],
    max_bytes: usize,
    block_align: usize,
) -> BoundedPush {
    if block_align == 0 {
        return BoundedPush {
            dropped_frames: (data.len() / block_align.max(1)) as u64,
        };
    }

    let cap = (max_bytes / block_align) * block_align;
    let incoming_bytes = (data.len() / block_align) * block_align;
    if cap == 0 {
        let dropped_frames = ((queue.len() + incoming_bytes) / block_align) as u64;
        queue.clear();
        return BoundedPush { dropped_frames };
    }

    // Drop old data first while there is no room for the incoming packet.
    let mut dropped_bytes = 0usize;
    while queue.len() + incoming_bytes > cap && !queue.is_empty() {
        let take = block_align.min(queue.len());
        queue.drain(..take);
        dropped_bytes += take;
    }

    // If the packet alone is larger than the queue, keep only its newest tail.
    if incoming_bytes > cap {
        let keep = cap;
        let skip = incoming_bytes - keep;
        dropped_bytes += skip;
        queue.clear();
        queue.extend(&data[skip..incoming_bytes]);
    } else {
        queue.extend(&data[..incoming_bytes]);
    }

    BoundedPush {
        dropped_frames: (dropped_bytes / block_align) as u64,
    }
}

#[cfg(any(windows, test))]
fn silence_byte(format: &PcmFormat) -> u8 {
    if format.sample_type == PcmSampleType::Int && format.stored_bits == 8 {
        128
    } else {
        0
    }
}

#[cfg(any(windows, test))]
fn copy_capture_packet(
    format: &PcmFormat,
    destination: &mut [u8],
    packet: Option<&[u8]>,
    frames: u32,
    silent: bool,
) -> Result<usize, AudioError> {
    let bytes = (frames as usize)
        .checked_mul(format.bytes_per_frame())
        .filter(|size| *size <= destination.len())
        .ok_or_else(|| {
            AudioError::new(
                AudioErrorKind::Runtime,
                "capture packet exceeds destination bounds",
            )
        })?;
    if silent {
        destination[..bytes].fill(silence_byte(format));
    } else {
        let data = packet.filter(|data| data.len() >= bytes).ok_or_else(|| {
            AudioError::new(
                AudioErrorKind::Runtime,
                "capture packet data is null or truncated",
            )
        })?;
        destination[..bytes].copy_from_slice(&data[..bytes]);
    }
    Ok(bytes)
}

/// Lists active render endpoints using native Core Audio.
///
/// Endpoint order follows Core Audio enumeration order. Friendly names fall
/// back to the endpoint description or stable ID when a property is missing.
#[cfg(windows)]
pub fn list_endpoints() -> Result<Vec<Endpoint>, AudioError> {
    platform::list_endpoints()
}

/// Starts a native mirror session.
///
/// The call returns after every stream has been initialised and started, or
/// with an actionable error before audio has started. The returned
/// [`MirrorSession`] owns the worker thread. `stop` requests cancellation;
/// dropping the session or calling `stop_and_wait` also joins the worker.
///
/// `status`, when supplied, receives `Started`, `Fatal`, and `Stopped`
/// transitions. Runtime statistics are always available through
/// [`MirrorSession::stats`].
///
/// Startup fails when the source is missing/not default, there are fewer than
/// two active outputs, targets are duplicated/missing, a target is the source,
/// or the source mix format is not a validated PCM/float layout.
#[cfg(windows)]
pub fn start_mirror(
    source_id: &str,
    target_ids: &[String],
    stop: Arc<AtomicBool>,
    status: Option<std::sync::mpsc::Sender<MirrorEvent>>,
) -> Result<MirrorSession, AudioError> {
    platform::start_mirror(source_id, target_ids, stop, status)
}

/// Non-Windows stub: native WASAPI is unavailable.
#[cfg(not(windows))]
pub fn list_endpoints() -> Result<Vec<Endpoint>, AudioError> {
    Err(AudioError::unsupported_platform())
}

/// Non-Windows stub: returns an explicit `UnsupportedPlatform` error.
#[cfg(not(windows))]
pub fn start_mirror(
    _source_id: &str,
    _target_ids: &[String],
    _stop: Arc<AtomicBool>,
    _status: Option<std::sync::mpsc::Sender<MirrorEvent>>,
) -> Result<MirrorSession, AudioError> {
    Err(AudioError::unsupported_platform())
}

/// Non-Windows placeholder. It can never be constructed by this crate because
/// `start_mirror` always fails on non-Windows targets.
#[cfg(not(windows))]
#[derive(Debug)]
pub struct MirrorSession {
    _private: (),
}

#[cfg(not(windows))]
impl MirrorSession {
    pub fn stats(&self) -> MirrorStats {
        MirrorStats::default()
    }

    pub fn is_running(&self) -> bool {
        false
    }

    pub fn stop(&self) {}

    pub fn stop_and_wait(self) -> MirrorStats {
        MirrorStats::default()
    }
}

/// RAII owner of the Windows mirror worker.
#[cfg(windows)]
#[derive(Debug)]
pub struct MirrorSession {
    stop: Arc<AtomicBool>,
    health: Arc<Mutex<MirrorStats>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

#[cfg(windows)]
impl MirrorSession {
    /// Returns a consistent snapshot of current health.
    pub fn stats(&self) -> MirrorStats {
        lock_stats(&self.health).clone()
    }

    pub fn is_running(&self) -> bool {
        self.worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
            && lock_stats(&self.health).running
    }

    /// Requests shutdown without blocking. The worker observes the flag within
    /// one 20 ms event timeout (or immediately after the current packet).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Requests shutdown and waits for the worker and all Core Audio objects to
    /// be released. Consumes the session.
    pub fn stop_and_wait(mut self) -> MirrorStats {
        self.stop();
        self.join_worker();
        self.stats()
    }

    fn join_worker(&mut self) {
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(windows)]
impl Drop for MirrorSession {
    fn drop(&mut self) {
        self.stop();
        self.join_worker();
    }
}

#[cfg(windows)]
fn lock_stats(health: &Arc<Mutex<MirrorStats>>) -> std::sync::MutexGuard<'_, MirrorStats> {
    match health.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::sync::mpsc::{self, RecvTimeoutError, Sender};
    use std::thread;
    use std::time::{Duration, Instant};
    use wasapi::{
        AudioClient, AudioRenderClient, BufferFlags, Device, DeviceEnumerator, DeviceState,
        Direction, Handle, SampleType, StreamMode, WasapiError, WaveFormat,
    };
    use windows::Win32::Media::Audio::IAudioCaptureClient;

    const BUFFER_DURATION_HNS: i64 = 200_000; // 20 ms requested shared-mode buffer.
    const EVENT_WAIT_MS: u32 = 20;
    const DEFAULT_CHECK_INTERVAL: Duration = Duration::from_secs(1);
    const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
    const MAX_QUEUE_BYTES: usize = 8 * 1024 * 1024;
    const MAX_CAPTURE_SECONDS: u32 = 4;

    struct ComGuard {
        initialized: bool,
    }

    impl ComGuard {
        fn new() -> Result<Self, AudioError> {
            let mta = wasapi::initialize_mta();
            if mta.is_ok() {
                return Ok(Self { initialized: true });
            }
            let sta = wasapi::initialize_sta();
            if sta.is_ok() {
                return Ok(Self { initialized: true });
            }
            Err(AudioError::new(
                AudioErrorKind::ComInitialization,
                format!(
                    "could not initialise COM for the audio thread (MTA {:#x}, STA {:#x})",
                    mta.0 as u32, sta.0 as u32
                ),
            ))
        }
    }

    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.initialized {
                wasapi::deinitialize();
            }
        }
    }

    fn map_wasapi(context: &str, error: WasapiError) -> AudioError {
        let kind = match &error {
            WasapiError::UnsupportedFormat | WasapiError::UnsupportedSubformat(_) => {
                AudioErrorKind::UnsupportedFormat
            }
            WasapiError::Windows(windows_error) => match windows_error.code().0 as u32 {
                0x8889_0004 | 0x8889_0026 => AudioErrorKind::DeviceInvalidated,
                0x8889_0008 => AudioErrorKind::UnsupportedFormat,
                _ => AudioErrorKind::Runtime,
            },
            _ => AudioErrorKind::Runtime,
        };

        let message = if kind == AudioErrorKind::DeviceInvalidated {
            format!(
                "{context}: the audio endpoint was removed, disabled, or its format changed. \
                 Select an available output and start mirroring again ({error})"
            )
        } else {
            format!("{context}: {error}")
        };
        AudioError::new(kind, message)
    }

    fn waveformat_to_pcm(format: &WaveFormat) -> Result<PcmFormat, AudioError> {
        let sample_type = match format
            .get_subformat()
            .map_err(|e| map_wasapi("reading source sample format", e))?
        {
            SampleType::Float => PcmSampleType::Float,
            SampleType::Int => PcmSampleType::Int,
        };
        let stored_bits = format.get_bitspersample();
        let valid_bits = match format.get_validbitspersample() {
            0 => stored_bits,
            valid => valid,
        };
        Ok(PcmFormat {
            sample_type,
            stored_bits,
            valid_bits,
            channels: format.get_nchannels(),
            sample_rate: format.get_samplespersec(),
            block_align: format.get_blockalign() as u16,
        })
    }

    fn enumerate(
        enumerator: &DeviceEnumerator,
    ) -> Result<(Vec<Endpoint>, Option<String>), AudioError> {
        let default_id = enumerator
            .get_default_device(&Direction::Render)
            .ok()
            .and_then(|device| device.get_id().ok());

        let collection = enumerator
            .get_device_collection(&Direction::Render)
            .map_err(|e| map_wasapi("enumerating active render endpoints", e))?;
        let count = collection
            .get_nbr_devices()
            .map_err(|e| map_wasapi("reading render endpoint count", e))?;

        let mut endpoints = Vec::with_capacity(count as usize);
        for index in 0..count {
            let device = collection
                .get_device_at_index(index)
                .map_err(|e| map_wasapi("opening an active render endpoint", e))?;
            let id = device
                .get_id()
                .map_err(|e| map_wasapi("reading render endpoint id", e))?;
            let name = device
                .get_friendlyname()
                .or_else(|_| device.get_description())
                .unwrap_or_else(|_| id.clone());
            let device_format = device.get_device_format().ok();
            let channels = device_format.as_ref().map(WaveFormat::get_nchannels);
            let sample_rate = device_format.as_ref().map(WaveFormat::get_samplespersec);
            endpoints.push(Endpoint {
                is_default: default_id.as_deref() == Some(id.as_str()),
                id,
                name,
                channels,
                sample_rate,
            });
        }
        Ok((endpoints, default_id))
    }

    pub(super) fn list_endpoints() -> Result<Vec<Endpoint>, AudioError> {
        let _com = ComGuard::new()?;
        let enumerator = DeviceEnumerator::new()
            .map_err(|e| map_wasapi("creating the Core Audio device enumerator", e))?;
        enumerate(&enumerator).map(|(endpoints, _)| endpoints)
    }

    // The dependency's slice helper dereferences GetBuffer's pointer before
    // examining SILENT. WASAPI permits null data for silent packets, so obtain
    // and release packets here with explicit flag and bounds checks.
    struct CaptureReader {
        client: IAudioCaptureClient,
        format: PcmFormat,
    }

    struct PacketRelease<'a> {
        client: &'a IAudioCaptureClient,
        frames: u32,
        active: bool,
    }

    impl PacketRelease<'_> {
        fn release(&mut self) -> Result<(), AudioError> {
            self.active = false;
            // SAFETY: this releases exactly the frames acquired on this thread.
            unsafe { self.client.ReleaseBuffer(self.frames) }.map_err(|error| {
                map_wasapi("releasing capture packet", WasapiError::Windows(error))
            })
        }
    }

    impl Drop for PacketRelease<'_> {
        fn drop(&mut self) {
            if self.active {
                let _ = self.release();
            }
        }
    }

    impl CaptureReader {
        fn get_next_packet_size(&self) -> Result<Option<u32>, AudioError> {
            // SAFETY: the live COM client belongs to the calling worker thread.
            unsafe { self.client.GetNextPacketSize() }
                .map(Some)
                .map_err(|error| {
                    map_wasapi("reading loopback packet size", WasapiError::Windows(error))
                })
        }

        fn read_from_device(
            &self,
            destination: &mut [u8],
        ) -> Result<(u32, BufferFlags), AudioError> {
            let mut pointer = std::ptr::null_mut();
            let mut frames = 0;
            let mut raw_flags = 0;
            // SAFETY: out-pointers are initialized, writable, and live until return.
            unsafe {
                self.client
                    .GetBuffer(&mut pointer, &mut frames, &mut raw_flags, None, None)
            }
            .map_err(|error| map_wasapi("acquiring capture packet", WasapiError::Windows(error)))?;
            let mut release = PacketRelease {
                client: &self.client,
                frames,
                active: true,
            };
            let flags = BufferFlags::new(raw_flags);
            let bytes = (frames as usize)
                .checked_mul(self.format.bytes_per_frame())
                .filter(|size| *size <= destination.len())
                .ok_or_else(|| {
                    AudioError::new(
                        AudioErrorKind::Runtime,
                        "capture packet exceeds allocated buffer",
                    )
                })?;
            let data = if flags.silent || bytes == 0 {
                None
            } else {
                if pointer.is_null() {
                    return Err(AudioError::new(
                        AudioErrorKind::Runtime,
                        "non-silent capture packet is null",
                    ));
                }
                // SAFETY: WASAPI owns this readable packet until ReleaseBuffer;
                // its frame count has been checked against the destination.
                Some(unsafe { std::slice::from_raw_parts(pointer, bytes) })
            };
            if bytes > 0 {
                copy_capture_packet(&self.format, destination, data, frames, flags.silent)?;
            }
            release.release()?;
            Ok((frames, flags))
        }
    }

    struct CaptureSource {
        // Service interfaces are released before their owning audio client.
        capture: CaptureReader,
        client: AudioClient,
        event: Handle,
        pcm: PcmFormat,
        buffer_frames: u32,
    }

    impl CaptureSource {
        fn stop(&self) {
            let _ = self.client.stop_stream();
        }
    }

    impl Drop for CaptureSource {
        fn drop(&mut self) {
            self.stop();
        }
    }

    struct RenderSink {
        endpoint: Endpoint,
        render: AudioRenderClient,
        client: AudioClient,
        _event: Handle,
        queue: VecDeque<u8>,
        max_bytes: usize,
        buffer_frames: u32,
        scratch: Vec<u8>,
        frames_rendered: u64,
        overflow_frames: u64,
        underrun_frames: u64,
    }

    impl RenderSink {
        fn stop(&self) {
            let _ = self.client.stop_stream();
        }
    }

    impl Drop for RenderSink {
        fn drop(&mut self) {
            self.stop();
        }
    }

    struct Prepared {
        enumerator: DeviceEnumerator,
        plan: MirrorPlan,
        source_device: Device,
        capture: CaptureSource,
        capture_scratch: Vec<u8>,
        target_devices: Vec<Device>,
        sinks: Vec<RenderSink>,
    }

    fn prepare(source_id: &str, target_ids: &[String]) -> Result<Prepared, AudioError> {
        let enumerator = DeviceEnumerator::new()
            .map_err(|e| map_wasapi("creating the Core Audio device enumerator", e))?;
        let (endpoints, default_id) = enumerate(&enumerator)?;
        let plan = select_mirror_plan(&endpoints, default_id.as_deref(), source_id, target_ids)?;

        let source_device = enumerator
            .get_device(&plan.source.id)
            .map_err(|e| map_wasapi("opening the source render endpoint", e))?;
        if source_device
            .get_state()
            .map_err(|e| map_wasapi("reading source endpoint state", e))?
            != DeviceState::Active
        {
            return Err(AudioError::new(
                AudioErrorKind::SourceMissing,
                format!(
                    "source render endpoint {:?} is no longer active; select an active default output",
                    plan.source.name
                ),
            ));
        }

        let mut source_client = source_device
            .get_iaudioclient()
            .map_err(|e| map_wasapi("opening the source audio client", e))?;
        let wave_format = source_client
            .get_mixformat()
            .map_err(|e| map_wasapi("reading the source mix format", e))?;
        let pcm = waveformat_to_pcm(&wave_format)?;
        validate_pcm_format(&pcm)?;

        source_client
            .initialize_client(
                &wave_format,
                &Direction::Capture,
                &StreamMode::EventsShared {
                    autoconvert: false,
                    buffer_duration_hns: BUFFER_DURATION_HNS,
                },
            )
            .map_err(|e| map_wasapi("initialising loopback capture in shared event mode", e))?;
        let capture_event = source_client
            .set_get_eventhandle()
            .map_err(|e| map_wasapi("creating the capture event handle", e))?;
        // SAFETY: the initialized source client exposes a capture service in
        // loopback mode; all interfaces remain on this COM-initialized thread.
        let capture = CaptureReader {
            client: unsafe { source_client.as_iaudioclient().GetService() }.map_err(|error| {
                map_wasapi("opening capture service", WasapiError::Windows(error))
            })?,
            format: pcm,
        };
        let capture_frames = source_client
            .get_buffer_size()
            .map_err(|e| map_wasapi("reading the capture buffer size", e))?;
        if capture_frames == 0 {
            return Err(AudioError::new(
                AudioErrorKind::Initialization,
                "Windows reported a zero-sized loopback capture buffer",
            ));
        }
        let max_capture_frames = pcm.sample_rate.saturating_mul(MAX_CAPTURE_SECONDS);
        if capture_frames > max_capture_frames {
            return Err(AudioError::new(
                AudioErrorKind::Initialization,
                format!(
                    "loopback capture buffer of {} frames exceeds the {} frame safety bound",
                    capture_frames, max_capture_frames
                ),
            ));
        }
        let frame_bytes = pcm.bytes_per_frame();
        let capture_bytes = (capture_frames as usize)
            .checked_mul(frame_bytes)
            .filter(|size| *size <= MAX_QUEUE_BYTES)
            .ok_or_else(|| {
                AudioError::new(
                    AudioErrorKind::Initialization,
                    "capture buffer exceeds memory safety limit",
                )
            })?;
        let capture_scratch = vec![silence_byte(&pcm); capture_bytes];

        let capture_source = CaptureSource {
            client: source_client,
            capture,
            event: capture_event,
            pcm,
            buffer_frames: capture_frames,
        };

        let queue_cap = (pcm.sample_rate as usize)
            .saturating_mul(frame_bytes)
            .min(MAX_QUEUE_BYTES)
            .max(frame_bytes);

        let mut target_devices = Vec::with_capacity(plan.targets.len());
        let mut sinks = Vec::with_capacity(plan.targets.len());
        for target in &plan.targets {
            let device = enumerator
                .get_device(&target.id)
                .map_err(|e| map_wasapi("opening a target render endpoint", e))?;
            if device
                .get_state()
                .map_err(|e| map_wasapi("reading target endpoint state", e))?
                != DeviceState::Active
            {
                return Err(AudioError::new(
                    AudioErrorKind::TargetMissing,
                    format!(
                        "target render endpoint {:?} is no longer active; select another output",
                        target.name
                    ),
                ));
            }

            let mut client = device
                .get_iaudioclient()
                .map_err(|e| map_wasapi("opening a target audio client", e))?;
            client
                .initialize_client(
                    &wave_format,
                    &Direction::Render,
                    &StreamMode::EventsShared {
                        autoconvert: true,
                        buffer_duration_hns: BUFFER_DURATION_HNS,
                    },
                )
                .map_err(|e| {
                    map_wasapi(
                        &format!(
                            "target {:?} cannot accept the source format {} Hz / {} channel(s) / {:?} {} bits; \
                             no conversion was attempted outside the Windows audio engine",
                            target.name,
                            pcm.sample_rate,
                            pcm.channels,
                            pcm.sample_type,
                            pcm.stored_bits
                        ),
                        e,
                    )
                })?;
            let event = client
                .set_get_eventhandle()
                .map_err(|e| map_wasapi("creating a target event handle", e))?;
            let render = client
                .get_audiorenderclient()
                .map_err(|e| map_wasapi("opening a target render client", e))?;
            let buffer_frames = client
                .get_buffer_size()
                .map_err(|e| map_wasapi("reading a target buffer size", e))?;
            if buffer_frames == 0 {
                return Err(AudioError::new(
                    AudioErrorKind::Initialization,
                    format!(
                        "target {:?} reported a zero-sized render buffer",
                        target.name
                    ),
                ));
            }
            let scratch_bytes = (buffer_frames as usize)
                .checked_mul(frame_bytes)
                .filter(|size| *size <= MAX_QUEUE_BYTES)
                .ok_or_else(|| {
                    AudioError::new(
                        AudioErrorKind::Initialization,
                        "render buffer exceeds memory safety limit",
                    )
                })?;
            let scratch = vec![silence_byte(&pcm); scratch_bytes];

            let mut sink = RenderSink {
                endpoint: target.clone(),
                client,
                render,
                _event: event,
                queue: VecDeque::with_capacity(queue_cap.min(64 * 1024)),
                max_bytes: queue_cap,
                buffer_frames,
                scratch,
                frames_rendered: 0,
                overflow_frames: 0,
                underrun_frames: 0,
            };
            prefill_silence(&mut sink)?;
            target_devices.push(device);
            sinks.push(sink);
        }

        // Every client is initialised and every format was accepted. Start
        // rendering first, then capture. Any failure drops the local values,
        // whose Drop implementations stop already-started streams.
        for sink in &sinks {
            sink.client
                .start_stream()
                .map_err(|e| map_wasapi("starting a target render stream", e))?;
        }
        capture_source
            .client
            .start_stream()
            .map_err(|e| map_wasapi("starting the loopback capture stream", e))?;

        Ok(Prepared {
            enumerator,
            plan,
            source_device,
            capture: capture_source,
            capture_scratch,
            target_devices,
            sinks,
        })
    }

    fn prefill_silence(sink: &mut RenderSink) -> Result<(), AudioError> {
        let available = sink
            .client
            .get_available_space_in_frames()
            .map_err(|e| map_wasapi("reading target buffer space before start", e))?;
        if available == 0 {
            return Ok(());
        }
        let frame_bytes = sink.scratch.len() / sink.buffer_frames.max(1) as usize;
        let bytes = (available as usize).saturating_mul(frame_bytes);
        if bytes > sink.scratch.len() {
            return Err(AudioError::new(
                AudioErrorKind::Initialization,
                "target buffer space exceeded the allocated scratch buffer",
            ));
        }
        for byte in &mut sink.scratch[..bytes] {
            *byte = 0;
        }
        let flags = BufferFlags {
            data_discontinuity: false,
            silent: true,
            timestamp_error: false,
        };
        sink.render
            .write_to_device(available as usize, &sink.scratch[..bytes], Some(flags))
            .map_err(|e| map_wasapi("priming a target render buffer with silence", e))
    }

    fn service_sink(sink: &mut RenderSink, format: &PcmFormat) -> Result<(), AudioError> {
        let frame_bytes = format.bytes_per_frame();
        let available = sink
            .client
            .get_available_space_in_frames()
            .map_err(|e| map_wasapi("reading target buffer space", e))?;
        if available == 0 {
            return Ok(());
        }
        let available = available as usize;
        let needed = available
            .checked_mul(frame_bytes)
            .ok_or_else(|| AudioError::new(AudioErrorKind::Runtime, "buffer size overflow"))?;
        if needed > sink.scratch.len() {
            return Err(AudioError::new(
                AudioErrorKind::Runtime,
                format!(
                    "target {:?} reported more writable frames than its buffer holds",
                    sink.endpoint.name
                ),
            ));
        }

        let queued_frames = sink.queue.len() / frame_bytes;
        let frames = queued_frames.min(available);
        let data_bytes = frames * frame_bytes;
        if data_bytes > 0 {
            let data = sink.queue.make_contiguous();
            sink.scratch[..data_bytes].copy_from_slice(&data[..data_bytes]);
            sink.queue.drain(..data_bytes);
        }
        let silent_bytes = needed - data_bytes;
        if silent_bytes > 0 {
            for byte in &mut sink.scratch[data_bytes..needed] {
                *byte = silence_byte(format);
            }
            sink.underrun_frames = sink
                .underrun_frames
                .saturating_add(silent_bytes as u64 / frame_bytes as u64);
        }

        let flags = BufferFlags {
            // ReleaseBuffer on a render client accepts only SILENT or zero.
            data_discontinuity: false,
            silent: frames == 0,
            timestamp_error: false,
        };
        sink.render
            .write_to_device(available, &sink.scratch[..needed], Some(flags))
            .map_err(|e| map_wasapi(&format!("writing to target {:?}", sink.endpoint.name), e))?;
        sink.frames_rendered = sink.frames_rendered.saturating_add(frames as u64);
        Ok(())
    }

    fn default_is_still_source(prepared: &Prepared) -> Result<(), AudioError> {
        let default = prepared
            .enumerator
            .get_default_device(&Direction::Render)
            .map_err(|e| map_wasapi("checking the current default render endpoint", e))?;
        let default_id = default
            .get_id()
            .map_err(|e| map_wasapi("reading the current default render endpoint id", e))?;
        if default_id != prepared.plan.source.id {
            return Err(AudioError::new(
                AudioErrorKind::SourceNotDefault,
                format!(
                    "the Windows default render output changed from {:?}; mirroring stopped rather than capturing a non-default source",
                    prepared.plan.source.name
                ),
            ));
        }
        Ok(())
    }

    fn check_devices(prepared: &Prepared) -> Result<(), AudioError> {
        if prepared
            .source_device
            .get_state()
            .map_err(|e| map_wasapi("checking source endpoint state", e))?
            != DeviceState::Active
        {
            return Err(AudioError::new(
                AudioErrorKind::DeviceInvalidated,
                format!(
                    "source render endpoint {:?} was removed or disabled; select an active output and start again",
                    prepared.plan.source.name
                ),
            ));
        }
        for (device, sink) in prepared.target_devices.iter().zip(&prepared.sinks) {
            if device
                .get_state()
                .map_err(|e| map_wasapi("checking target endpoint state", e))?
                != DeviceState::Active
            {
                return Err(AudioError::new(
                    AudioErrorKind::DeviceInvalidated,
                    format!(
                        "target render endpoint {:?} was removed or disabled; select an active output and start again",
                        sink.endpoint.name
                    ),
                ));
            }
        }
        default_is_still_source(prepared)
    }

    fn sync_health(
        health: &Arc<Mutex<MirrorStats>>,
        prepared: &Prepared,
        frames_captured: u64,
        running: bool,
        error: Option<&AudioError>,
    ) {
        let mut stats = lock_stats(health);
        stats.running = running;
        stats.frames_captured = frames_captured;
        stats.frames_rendered = prepared.sinks.iter().map(|s| s.frames_rendered).sum();
        stats.overflow_frames = prepared.sinks.iter().map(|s| s.overflow_frames).sum();
        stats.underrun_frames = prepared.sinks.iter().map(|s| s.underrun_frames).sum();
        stats.targets = prepared
            .sinks
            .iter()
            .map(|sink| TargetHealth {
                id: sink.endpoint.id.clone(),
                name: sink.endpoint.name.clone(),
                frames_rendered: sink.frames_rendered,
                overflow_frames: sink.overflow_frames,
                underrun_frames: sink.underrun_frames,
                last_error: error.map(|e| e.message().to_owned()),
            })
            .collect();
        if let Some(error) = error {
            stats.last_error = Some(error.message().to_owned());
        }
    }

    fn run_loop(
        prepared: &mut Prepared,
        stop: &AtomicBool,
        health: &Arc<Mutex<MirrorStats>>,
        status: &Option<Sender<MirrorEvent>>,
    ) {
        let frame_bytes = prepared.capture.pcm.bytes_per_frame();
        let mut frames_captured: u64 = 0;
        let mut last_check = Instant::now();
        let mut fatal: Option<AudioError> = None;

        'outer: loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }

            // Drain every packet that the loopback capture client currently
            // has. Each packet is copied into every bounded per-target queue.
            loop {
                if stop.load(Ordering::Relaxed) {
                    break 'outer;
                }
                match prepared.capture.capture.get_next_packet_size() {
                    Ok(None) | Ok(Some(0)) => break,
                    Ok(Some(frames)) => {
                        let frames = frames as usize;
                        let bytes = match frames.checked_mul(frame_bytes) {
                            Some(bytes) if bytes <= prepared.capture_scratch.len() => bytes,
                            _ => {
                                fatal = Some(AudioError::new(
                                    AudioErrorKind::Runtime,
                                    format!(
                                        "loopback packet of {} frames exceeds the {} frame capture buffer",
                                        frames,
                                        prepared.capture.buffer_frames
                                    ),
                                ));
                                break 'outer;
                            }
                        };

                        let (read, _flags) = match prepared
                            .capture
                            .capture
                            .read_from_device(&mut prepared.capture_scratch[..bytes])
                        {
                            Ok(value) => value,
                            Err(error) => {
                                fatal = Some(error);
                                break 'outer;
                            }
                        };
                        if read == 0 {
                            continue;
                        }
                        let actual_bytes = read as usize * frame_bytes;
                        if actual_bytes > bytes {
                            fatal = Some(AudioError::new(
                                AudioErrorKind::Runtime,
                                "loopback capture returned more frames than requested",
                            ));
                            break 'outer;
                        }

                        let source = &prepared.capture_scratch[..actual_bytes];
                        frames_captured = frames_captured.saturating_add(read as u64);
                        for sink in &mut prepared.sinks {
                            let pushed = push_bounded_frames(
                                &mut sink.queue,
                                source,
                                sink.max_bytes,
                                frame_bytes,
                            );
                            if pushed.dropped_frames > 0 {
                                sink.overflow_frames =
                                    sink.overflow_frames.saturating_add(pushed.dropped_frames);
                            }
                        }
                    }
                    Err(error) => {
                        fatal = Some(error);
                        break 'outer;
                    }
                }
            }

            for sink in &mut prepared.sinks {
                if let Err(error) = service_sink(sink, &prepared.capture.pcm) {
                    fatal = Some(error);
                    break 'outer;
                }
            }

            sync_health(health, prepared, frames_captured, true, None);

            if last_check.elapsed() >= DEFAULT_CHECK_INTERVAL {
                if let Err(error) = check_devices(prepared) {
                    fatal = Some(error);
                    break;
                }
                last_check = Instant::now();
            }

            // Event-driven wait with a bounded timeout. The timeout lets the
            // worker observe the stop flag and service underruns without a
            // busy spin.
            match prepared.capture.event.wait_for_event(EVENT_WAIT_MS) {
                Ok(()) | Err(WasapiError::EventTimeout) => {}
                Err(error) => {
                    fatal = Some(map_wasapi("waiting for loopback capture audio", error));
                    break;
                }
            }
        }

        if let Some(error) = &fatal {
            stop.store(true, Ordering::SeqCst);
            sync_health(health, prepared, frames_captured, false, Some(error));
            if let Some(sender) = status {
                let _ = sender.send(MirrorEvent::Fatal {
                    error: error.clone(),
                });
            }
        } else {
            sync_health(health, prepared, frames_captured, false, None);
        }
    }

    fn run_worker(
        source_id: String,
        target_ids: Vec<String>,
        stop: Arc<AtomicBool>,
        health: Arc<Mutex<MirrorStats>>,
        status: Option<Sender<MirrorEvent>>,
        init: Sender<Result<MirrorPlan, AudioError>>,
    ) {
        let _com = match ComGuard::new() {
            Ok(com) => com,
            Err(error) => {
                {
                    let mut stats = lock_stats(&health);
                    stats.running = false;
                    stats.last_error = Some(error.message().to_owned());
                }
                let _ = init.send(Err(error.clone()));
                if let Some(sender) = &status {
                    let _ = sender.send(MirrorEvent::Fatal { error });
                }
                return;
            }
        };

        let mut prepared = match prepare(&source_id, &target_ids) {
            Ok(prepared) => prepared,
            Err(error) => {
                {
                    let mut stats = lock_stats(&health);
                    stats.running = false;
                    stats.last_error = Some(error.message().to_owned());
                }
                let _ = init.send(Err(error.clone()));
                if let Some(sender) = &status {
                    let _ = sender.send(MirrorEvent::Fatal { error });
                }
                return;
            }
        };

        {
            let mut stats = lock_stats(&health);
            stats.running = true;
            stats.targets = prepared
                .sinks
                .iter()
                .map(|sink| TargetHealth {
                    id: sink.endpoint.id.clone(),
                    name: sink.endpoint.name.clone(),
                    frames_rendered: 0,
                    overflow_frames: 0,
                    underrun_frames: 0,
                    last_error: None,
                })
                .collect();
        }

        if init.send(Ok(prepared.plan.clone())).is_err() {
            stop.store(true, Ordering::SeqCst);
            return;
        }
        if let Some(sender) = &status {
            let _ = sender.send(MirrorEvent::Started {
                source: prepared.plan.source.clone(),
                targets: prepared.plan.targets.clone(),
            });
        }

        run_loop(&mut prepared, &stop, &health, &status);

        if let Some(sender) = &status {
            let _ = sender.send(MirrorEvent::Stopped);
        }
    }

    pub(super) fn start_mirror(
        source_id: &str,
        target_ids: &[String],
        stop: Arc<AtomicBool>,
        status: Option<Sender<MirrorEvent>>,
    ) -> Result<MirrorSession, AudioError> {
        if stop.load(Ordering::SeqCst) {
            return Err(AudioError::new(
                AudioErrorKind::Runtime,
                "mirror start refused because the stop flag was already set",
            ));
        }

        let health = Arc::new(Mutex::new(MirrorStats::default()));
        let (init_tx, init_rx) = mpsc::channel::<Result<MirrorPlan, AudioError>>();
        let worker_stop = stop.clone();
        let worker_health = health.clone();
        let source = source_id.to_owned();
        let targets = target_ids.to_vec();

        let panic_health = health.clone();
        let panic_stop = stop.clone();
        let panic_status = status.clone();
        let panic_init = init_tx.clone();
        let worker = thread::Builder::new()
            .name("audiomix-wasapi-mirror".to_owned())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_worker(source, targets, worker_stop, worker_health, status, init_tx)
                }));
                if outcome.is_err() {
                    panic_stop.store(true, Ordering::SeqCst);
                    let error = AudioError::new(
                        AudioErrorKind::Runtime,
                        "WASAPI worker failed internally; mirroring stopped",
                    );
                    let mut stats = lock_stats(&panic_health);
                    stats.running = false;
                    stats.last_error = Some(error.to_string());
                    drop(stats);
                    let _ = panic_init.send(Err(error.clone()));
                    if let Some(sender) = panic_status {
                        let _ = sender.send(MirrorEvent::Fatal { error });
                        let _ = sender.send(MirrorEvent::Stopped);
                    }
                }
            })
            .map_err(|error| {
                AudioError::new(
                    AudioErrorKind::Runtime,
                    format!("could not spawn the WASAPI mirror worker: {error}"),
                )
            })?;

        let mut worker = Some(worker);
        match init_rx.recv_timeout(STARTUP_TIMEOUT) {
            Ok(Ok(_plan)) => {}
            Ok(Err(error)) => {
                stop.store(true, Ordering::SeqCst);
                if let Some(handle) = worker.take() {
                    let _ = handle.join();
                }
                return Err(error);
            }
            Err(RecvTimeoutError::Timeout) => {
                stop.store(true, Ordering::SeqCst);
                if let Some(handle) = worker.take() {
                    let _ = handle.join();
                }
                return Err(AudioError::new(
                    AudioErrorKind::Timeout,
                    "timed out while initialising WASAPI streams; no mirror session was started",
                ));
            }
            Err(RecvTimeoutError::Disconnected) => {
                if let Some(handle) = worker.take() {
                    let _ = handle.join();
                }
                let message = lock_stats(&health)
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "WASAPI mirror worker stopped during startup".to_owned());
                return Err(AudioError::new(AudioErrorKind::Initialization, message));
            }
        };

        // The worker validates again with the live endpoint list and only
        // reports success after every stream has started. Health is the
        // authoritative runtime view.
        Ok(MirrorSession {
            stop,
            health,
            worker,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_packets_do_not_require_readable_data_and_unsigned_pcm_is_centered() {
        let pcm = PcmFormat {
            sample_type: PcmSampleType::Int,
            stored_bits: 8,
            valid_bits: 8,
            channels: 1,
            sample_rate: 48_000,
            block_align: 1,
        };
        let mut output = [7; 4];
        assert_eq!(
            copy_capture_packet(&pcm, &mut output, None, 4, true).unwrap(),
            4
        );
        assert_eq!(output, [128; 4]);
        assert!(copy_capture_packet(&pcm, &mut output, None, 4, false).is_err());
    }

    #[test]
    fn float_silence_and_capture_bounds_are_preserved() {
        let pcm = PcmFormat {
            sample_type: PcmSampleType::Float,
            stored_bits: 32,
            valid_bits: 32,
            channels: 2,
            sample_rate: 48_000,
            block_align: 8,
        };
        let mut output = [9; 16];
        assert!(copy_capture_packet(&pcm, &mut output, None, 2, true).is_ok());
        assert_eq!(output, [0; 16]);
        assert!(copy_capture_packet(&pcm, &mut output, Some(&[0; 8]), 2, false).is_err());
        assert!(copy_capture_packet(&pcm, &mut output, None, 3, true).is_err());
        assert_eq!(
            copy_capture_packet(&pcm, &mut output, Some(&[3; 16]), 2, false).unwrap(),
            16
        );
        assert_eq!(output, [3; 16]);
    }

    #[test]
    fn zero_capacity_discards_existing_audio_instead_of_retaining_a_hidden_queue() {
        let mut queue = VecDeque::from([1, 2, 3, 4]);
        let result = push_bounded_frames(&mut queue, &[5, 6], 0, 2);
        assert_eq!(result.dropped_frames, 3);
        assert!(queue.is_empty());
    }

    fn endpoint(id: &str, name: &str, is_default: bool) -> Endpoint {
        Endpoint {
            id: id.to_owned(),
            name: name.to_owned(),
            is_default,
            channels: Some(2),
            sample_rate: Some(48_000),
        }
    }

    fn three_endpoints() -> Vec<Endpoint> {
        vec![
            endpoint("default-id", "Default Speakers", true),
            endpoint("hdmi-id", "HDMI", false),
            endpoint("usb-id", "USB Headset", false),
        ]
    }

    fn targets(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn selects_default_source_and_secondary_targets() {
        let plan = select_mirror_plan(
            &three_endpoints(),
            Some("default-id"),
            "default-id",
            &targets(&["hdmi-id", "usb-id"]),
        )
        .expect("valid plan");
        assert_eq!(plan.source.id, "default-id");
        assert_eq!(plan.targets.len(), 2);
        assert!(plan.targets.iter().all(|t| t.id != plan.source.id));
    }

    #[test]
    fn rejects_non_default_source() {
        let error = select_mirror_plan(
            &three_endpoints(),
            Some("default-id"),
            "hdmi-id",
            &targets(&["usb-id"]),
        )
        .expect_err("non-default source");
        assert_eq!(error.kind(), AudioErrorKind::SourceNotDefault);
    }

    #[test]
    fn rejects_no_default_source() {
        let error = select_mirror_plan(
            &three_endpoints(),
            None,
            "default-id",
            &targets(&["usb-id"]),
        )
        .expect_err("no default");
        assert_eq!(error.kind(), AudioErrorKind::SourceNotDefault);
    }

    #[test]
    fn rejects_fewer_than_two_outputs() {
        let error = select_mirror_plan(
            &[endpoint("only-id", "Only", true)],
            Some("only-id"),
            "only-id",
            &targets(&["only-id"]),
        )
        .expect_err("few outputs");
        assert_eq!(error.kind(), AudioErrorKind::TooFewOutputs);
    }

    #[test]
    fn rejects_duplicate_targets() {
        let error = select_mirror_plan(
            &three_endpoints(),
            Some("default-id"),
            "default-id",
            &targets(&["hdmi-id", "hdmi-id"]),
        )
        .expect_err("duplicate targets");
        assert_eq!(error.kind(), AudioErrorKind::DuplicateTarget);
    }

    #[test]
    fn rejects_source_as_target() {
        let error = select_mirror_plan(
            &three_endpoints(),
            Some("default-id"),
            "default-id",
            &targets(&["default-id"]),
        )
        .expect_err("source as target");
        assert_eq!(error.kind(), AudioErrorKind::TargetIsSource);
    }

    #[test]
    fn rejects_missing_target() {
        let error = select_mirror_plan(
            &three_endpoints(),
            Some("default-id"),
            "default-id",
            &targets(&["ghost-id"]),
        )
        .expect_err("missing target");
        assert_eq!(error.kind(), AudioErrorKind::TargetMissing);
    }

    #[test]
    fn rejects_empty_target_list() {
        let error = select_mirror_plan(&three_endpoints(), Some("default-id"), "default-id", &[])
            .expect_err("no targets");
        assert_eq!(error.kind(), AudioErrorKind::NoTargets);
    }

    #[test]
    fn rejects_duplicate_endpoint_ids() {
        let error = select_mirror_plan(
            &[
                endpoint("same-id", "A", true),
                endpoint("same-id", "B", false),
                endpoint("other-id", "C", false),
            ],
            Some("same-id"),
            "same-id",
            &targets(&["other-id"]),
        )
        .expect_err("duplicate endpoints");
        assert_eq!(error.kind(), AudioErrorKind::DuplicateEndpoint);
    }

    fn pcm(sample_type: PcmSampleType, stored_bits: u16, valid_bits: u16) -> PcmFormat {
        PcmFormat {
            sample_type,
            stored_bits,
            valid_bits,
            channels: 2,
            sample_rate: 48_000,
            block_align: 2 * stored_bits.div_ceil(8),
        }
    }

    #[test]
    fn accepts_float32_and_pcm16() {
        assert!(validate_pcm_format(&pcm(PcmSampleType::Float, 32, 32)).is_ok());
        assert!(validate_pcm_format(&pcm(PcmSampleType::Int, 16, 16)).is_ok());
        assert!(validate_pcm_format(&pcm(PcmSampleType::Int, 24, 24)).is_ok());
        assert!(validate_pcm_format(&pcm(PcmSampleType::Int, 32, 24)).is_ok());
    }

    #[test]
    fn rejects_unsupported_sample_layouts() {
        assert_eq!(
            validate_pcm_format(&pcm(PcmSampleType::Float, 16, 16))
                .expect_err("float16")
                .kind(),
            AudioErrorKind::UnsupportedFormat
        );
        assert_eq!(
            validate_pcm_format(&pcm(PcmSampleType::Int, 20, 20))
                .expect_err("pcm20")
                .kind(),
            AudioErrorKind::UnsupportedFormat
        );
        let mut bad_valid = pcm(PcmSampleType::Int, 32, 20);
        bad_valid.valid_bits = 20;
        assert_eq!(
            validate_pcm_format(&bad_valid)
                .expect_err("pcm20-in-32")
                .kind(),
            AudioErrorKind::UnsupportedFormat
        );
    }

    #[test]
    fn rejects_bad_block_alignment_instead_of_reinterpreting() {
        let mut bad = pcm(PcmSampleType::Int, 16, 16);
        bad.block_align = 3;
        assert_eq!(
            validate_pcm_format(&bad)
                .expect_err("bad block align")
                .kind(),
            AudioErrorKind::UnsupportedFormat
        );

        let mut zero_channels = pcm(PcmSampleType::Float, 32, 32);
        zero_channels.channels = 0;
        zero_channels.block_align = 0;
        assert_eq!(
            validate_pcm_format(&zero_channels)
                .expect_err("zero channels")
                .kind(),
            AudioErrorKind::UnsupportedFormat
        );
    }

    #[test]
    fn bounded_queue_drops_oldest_whole_frames() {
        let mut queue = VecDeque::new();
        let cap = 8;
        assert_eq!(
            push_bounded_frames(&mut queue, &[1, 1, 1, 1], cap, 2),
            BoundedPush { dropped_frames: 0 }
        );
        assert_eq!(
            push_bounded_frames(&mut queue, &[2, 2, 2, 2], cap, 2),
            BoundedPush { dropped_frames: 0 }
        );
        assert_eq!(queue.len(), 8);
        assert_eq!(
            push_bounded_frames(&mut queue, &[3, 3, 3, 3], cap, 2),
            BoundedPush { dropped_frames: 2 }
        );
        assert_eq!(queue.len(), 8);
        assert_eq!(
            queue.iter().copied().collect::<Vec<_>>(),
            vec![2, 2, 2, 2, 3, 3, 3, 3]
        );
    }

    #[test]
    fn bounded_queue_keeps_newest_tail_of_oversized_packet() {
        let mut queue = VecDeque::new();
        let result = push_bounded_frames(&mut queue, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10], 6, 2);
        assert_eq!(result, BoundedPush { dropped_frames: 2 });
        assert_eq!(queue.len(), 6);
        assert_eq!(
            queue.iter().copied().collect::<Vec<_>>(),
            vec![5, 6, 7, 8, 9, 10]
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_entry_points_are_explicitly_unsupported() {
        let error = list_endpoints().expect_err("unsupported");
        assert_eq!(error.kind(), AudioErrorKind::UnsupportedPlatform);
        let error = start_mirror(
            "source",
            &["target".to_owned()],
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .expect_err("unsupported");
        assert_eq!(error.kind(), AudioErrorKind::UnsupportedPlatform);
    }
}
