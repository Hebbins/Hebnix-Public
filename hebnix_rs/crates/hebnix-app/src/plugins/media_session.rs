//! whatever windows reports as "now playing" (the media flyout), via the
//! system media transport controls. a worker thread polls it and caches a
//! snapshot so lua reads are just a mutex lock. it only reports while
//! rocket league is running

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Storage::Streams::DataReader;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};

const POLL: Duration = Duration::from_millis(750);
// 100ns ticks between 1601-01-01 and 1970-01-01
const UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;
// covers bigger than this are almost certainly junk
const MAX_THUMB: u64 = 16 * 1024 * 1024;

#[derive(Clone, Default)]
pub struct MediaSnapshot {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub app: String,
    pub status: &'static str,
    pub position_ms: i64,
    pub duration_ms: i64,
    pub updated_unix_ms: i64,
    pub thumb_rev: u64,
}

#[derive(Default)]
struct Shared {
    snap: Option<MediaSnapshot>,
    thumb: Option<Vec<u8>>,
}

fn shared() -> &'static Mutex<Shared> {
    static SHARED: OnceLock<Mutex<Shared>> = OnceLock::new();
    SHARED.get_or_init(|| {
        std::thread::Builder::new()
            .name("media-session".into())
            .spawn(worker)
            .ok();
        Mutex::new(Shared::default())
    })
}

/// latest now-playing info, None when nothing has a media session
pub fn snapshot() -> Option<MediaSnapshot> {
    shared().lock().ok()?.snap.clone()
}

/// raw cover bytes for the current snapshot's thumb_rev, None until one loads
pub fn thumbnail() -> Option<Vec<u8>> {
    shared().lock().ok()?.thumb.clone()
}

fn status_name(s: Status) -> &'static str {
    match s {
        Status::Playing => "playing",
        Status::Paused => "paused",
        Status::Stopped => "stopped",
        Status::Changing => "changing",
        Status::Opened => "opened",
        _ => "closed",
    }
}

fn read_thumb(session: &Session) -> Option<Vec<u8>> {
    let props = session.TryGetMediaPropertiesAsync().ok()?.join().ok()?;
    let stream = props.Thumbnail().ok()?.OpenReadAsync().ok()?.join().ok()?;
    let size = stream.Size().ok()?;
    if size == 0 || size > MAX_THUMB {
        return None;
    }
    let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0).ok()?).ok()?;
    let got = reader.LoadAsync(size as u32).ok()?.join().ok()?;
    let mut buf = vec![0u8; got as usize];
    reader.ReadBytes(&mut buf).ok()?;
    Some(square_crop(buf))
}

// youtube and friends hand over 16:9 frames, the overlay draws covers square,
// so center-crop rather than let them get squashed. undecodable bytes pass through
fn square_crop(bytes: Vec<u8>) -> Vec<u8> {
    let Ok(img) = image::load_from_memory(&bytes) else {
        return bytes;
    };
    let (w, h) = (img.width(), img.height());
    if w == h {
        return bytes;
    }
    let side = w.min(h);
    let cropped = img.crop_imm((w - side) / 2, (h - side) / 2, side, side);
    let mut out = std::io::Cursor::new(Vec::new());
    match cropped.write_to(&mut out, image::ImageFormat::Png) {
        Ok(()) => out.into_inner(),
        Err(_) => bytes,
    }
}

// one poll: (snapshot without thumb_rev, track key used to tell when the cover changes)
fn read_session(session: &Session) -> Option<(MediaSnapshot, String)> {
    let props = session.TryGetMediaPropertiesAsync().ok()?.join().ok()?;
    let s = |r: windows::core::Result<windows::core::HSTRING>| {
        r.map(|h| h.to_string_lossy()).unwrap_or_default()
    };
    let title = s(props.Title());
    if title.is_empty() {
        return None;
    }
    let status = session
        .GetPlaybackInfo()
        .and_then(|i| i.PlaybackStatus())
        .map(status_name)
        .unwrap_or("closed");
    let (position_ms, duration_ms, updated_unix_ms) = match session.GetTimelineProperties() {
        Ok(tl) => {
            let start = tl.StartTime().map(|t| t.Duration).unwrap_or(0);
            let end = tl.EndTime().map(|t| t.Duration).unwrap_or(0);
            let pos = tl.Position().map(|t| t.Duration).unwrap_or(0);
            let upd = tl.LastUpdatedTime().map(|t| t.UniversalTime).unwrap_or(0);
            let upd_ms = if upd > UNIX_EPOCH_TICKS {
                (upd - UNIX_EPOCH_TICKS) / 10_000
            } else {
                0
            };
            ((pos - start).max(0) / 10_000, (end - start).max(0) / 10_000, upd_ms)
        }
        Err(_) => (0, 0, 0),
    };
    let snap = MediaSnapshot {
        artist: s(props.Artist()),
        album: s(props.AlbumTitle()),
        album_artist: s(props.AlbumArtist()),
        app: s(session.SourceAppUserModelId()),
        title,
        status,
        position_ms,
        duration_ms,
        updated_unix_ms,
        thumb_rev: 0,
    };
    let key = format!("{}\u{1}{}\u{1}{}", snap.app, snap.title, snap.artist);
    Some((snap, key))
}

fn worker() {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    let manager = loop {
        match SessionManager::RequestAsync().and_then(|op| op.join()) {
            Ok(m) => break m,
            Err(e) => {
                tracing::warn!("media session manager unavailable: {e}");
                std::thread::sleep(Duration::from_secs(10));
            }
        }
    };

    let mut last_key = String::new();
    let mut thumb_rev = 0u64;
    let mut thumb_tries = 0u8;
    loop {
        // only report media while rocket league is running, and don't touch
        // the session manager at all otherwise
        if !hebnix_sdk::process::is_rocket_league_running() {
            last_key.clear();
            thumb_tries = 0;
            if let Ok(mut sh) = shared().lock() {
                sh.snap = None;
                sh.thumb = None;
            }
            std::thread::sleep(POLL);
            continue;
        }
        let read = manager
            .GetCurrentSession()
            .ok()
            .and_then(|session| read_session(&session).map(|r| (session, r)));
        // Some(None) drops the old cover, Some(Some(..)) swaps in a new one
        let mut new_thumb: Option<Option<Vec<u8>>> = None;
        let snap = read.map(|(session, (mut snap, key))| {
            if key != last_key {
                last_key = key;
                thumb_tries = 0;
                thumb_rev += 1;
                new_thumb = Some(None);
            }
            // browsers often hand the cover over a beat after the title, so
            // keep retrying a few polls until one shows up
            if thumb_tries < 6 {
                thumb_tries += 1;
                if let Some(bytes) = read_thumb(&session) {
                    thumb_tries = u8::MAX;
                    thumb_rev += 1;
                    new_thumb = Some(Some(bytes));
                }
            }
            snap.thumb_rev = thumb_rev;
            snap
        });
        if snap.is_none() {
            last_key.clear();
        }
        if let Ok(mut sh) = shared().lock() {
            if let Some(t) = new_thumb {
                sh.thumb = t;
            } else if snap.is_none() {
                sh.thumb = None;
            }
            sh.snap = snap;
        }
        std::thread::sleep(POLL);
    }
}
