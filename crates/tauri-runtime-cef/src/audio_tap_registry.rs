//! Global audio-tap registry shared between the CEF audio handler (which fires
//! on the CEF IO/render thread) and the Tauri shell (which subscribes on the
//! async runtime).
//!
//! Uses `std::sync::mpsc` so we don't need a tokio dependency inside
//! `tauri-runtime-cef`. The Tauri shell wraps the `Receiver` in a helper that
//! bridges it to async using `tokio::task::spawn_blocking`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, mpsc as std_mpsc};

const LOG_PREFIX: &str = "[audio-tap]";

/// Maximum mono samples in the ring buffer per browser (30 s at 48 kHz).
const RING_MAX: usize = 48_000 * 30;

// ──────────────────────────────────────────────────────────────────────────────

pub struct AudioTapState {
  pub sample_rate: i32,
  pub channels: i32,
  pub active: bool,
  pub sender: Option<std_mpsc::SyncSender<Vec<f32>>>,
  pub ring_buffer: Vec<f32>,
}

static TAPS: OnceLock<Mutex<HashMap<i32, AudioTapState>>> = OnceLock::new();

fn taps() -> &'static Mutex<HashMap<i32, AudioTapState>> {
  TAPS.get_or_init(|| Mutex::new(HashMap::new()))
}

// ── Called from CEF audio handler ────────────────────────────────────────────

pub fn register_audio_tap(browser_id: i32, sample_rate: i32, channels: i32) {
  log::debug!(
    "{LOG_PREFIX} register browser_id={browser_id} sample_rate={sample_rate} channels={channels}"
  );
  let mut map = taps().lock().unwrap();
  let entry = map.entry(browser_id).or_insert_with(|| AudioTapState {
    sample_rate,
    channels,
    active: false,
    sender: None,
    ring_buffer: Vec::new(),
  });
  entry.sample_rate = sample_rate;
  entry.channels = channels;
  entry.active = true;
  log::info!(
    "{LOG_PREFIX} audio tap active browser_id={browser_id} sample_rate={sample_rate} channels={channels}"
  );
}

/// # Safety
/// `data` is a CEF-provided pointer valid only for the duration of the
/// `on_audio_stream_packet` callback.
pub unsafe fn push_audio_data(browser_id: i32, data: *mut *const f32, frames: i32, channels: i32) {
  if frames <= 0 || channels <= 0 || data.is_null() {
    return;
  }
  let frames = frames as usize;
  let ch = channels as usize;

  // Downmix to mono (average across channels, planar layout).
  let mut mono: Vec<f32> = Vec::with_capacity(frames);
  for i in 0..frames {
    let mut sum = 0.0f32;
    for c in 0..ch {
      let ch_ptr = unsafe { *data.add(c) };
      if ch_ptr.is_null() {
        continue;
      }
      sum += unsafe { *ch_ptr.add(i) };
    }
    mono.push(sum / ch as f32);
  }

  let mut map = taps().lock().unwrap();
  let Some(state) = map.get_mut(&browser_id) else {
    return;
  };
  if !state.active {
    return;
  }

  // Sliding ring buffer.
  state.ring_buffer.extend_from_slice(&mono);
  let excess = state.ring_buffer.len().saturating_sub(RING_MAX);
  if excess > 0 {
    state.ring_buffer.drain(..excess);
  }

  // Forward to subscriber (non-blocking send — drop chunk if full).
  if let Some(tx) = &state.sender {
    match tx.try_send(mono) {
      Ok(()) => {}
      Err(std_mpsc::TrySendError::Full(_)) => {
        log::trace!("{LOG_PREFIX} channel full browser_id={browser_id}");
      }
      Err(std_mpsc::TrySendError::Disconnected(_)) => {
        log::debug!("{LOG_PREFIX} channel disconnected browser_id={browser_id}");
        state.sender = None;
      }
    }
  }
}

pub fn unregister_audio_tap(browser_id: i32) {
  log::info!("{LOG_PREFIX} unregister browser_id={browser_id}");
  let mut map = taps().lock().unwrap();
  if let Some(state) = map.get_mut(&browser_id) {
    state.active = false;
    state.sender = None;
  }
}

// ── Called from Tauri shell ───────────────────────────────────────────────────

/// Subscribe to the audio stream for `browser_id`.
///
/// Returns a `std::sync::mpsc::Receiver` of mono f32 chunks, or `None` if
/// no tap is registered. Bounded channel with 64 slots (~seconds of headroom
/// at typical frames-per-buffer sizes).
pub fn subscribe_audio(browser_id: i32) -> Option<std_mpsc::Receiver<Vec<f32>>> {
  const CAP: usize = 64;
  let mut map = taps().lock().unwrap();
  let state = map.get_mut(&browser_id)?;
  let (tx, rx) = std_mpsc::sync_channel(CAP);
  state.sender = Some(tx);
  log::info!(
    "{LOG_PREFIX} subscribed browser_id={browser_id} sample_rate={} channels={}",
    state.sample_rate,
    state.channels
  );
  Some(rx)
}

/// Return the channel count for `browser_id`.
pub fn get_channels(browser_id: i32) -> Option<i32> {
  taps().lock().unwrap().get(&browser_id).map(|s| s.channels)
}

/// Return the sample rate for `browser_id`.
pub fn get_sample_rate(browser_id: i32) -> Option<i32> {
  taps()
    .lock()
    .unwrap()
    .get(&browser_id)
    .map(|s| s.sample_rate)
}

/// Return a snapshot clone of the ring buffer for `browser_id`.
/// Returns an empty `Vec` if no tap is registered.
pub fn snapshot_ring_buffer(browser_id: i32) -> Vec<f32> {
  taps()
    .lock()
    .unwrap()
    .get(&browser_id)
    .map(|s| s.ring_buffer.clone())
    .unwrap_or_default()
}
