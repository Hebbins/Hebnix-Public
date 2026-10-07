//! toasts. plugins push, the game overlay shows them, the notifications tab lists them.
//! history is capped and waiting is one entry per plugin, so spam cant grow memory.
//! all of it lives in memory only, closing hebnix wipes it.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use windows::Win32::System::SystemInformation::GetLocalTime;

use crate::i18n::{t, t_args};
use crate::overlay::{self, Rgba};

pub const HISTORY_MAX: usize = 150;
const TEXT_MAX: usize = 2000;
const PREVIEW_MAX: usize = 90;
const DEF_SECS: f32 = 4.0;
const MIN_SECS: f32 = 1.5;
const MAX_SECS: f32 = 5.0;
const FADE_IN: f32 = 0.25;
const FADE_OUT: f32 = 0.35;

const DEF_ACCENT: [u8; 4] = [88, 166, 255, 255];
const DEF_BG: [u8; 4] = [20, 20, 26, 235];
const DEF_TEXT: [u8; 4] = [255, 255, 255, 255];
const DEF_BODY: [u8; 4] = [190, 190, 200, 255];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToastPos {
    TopLeft,
    #[default]
    TopRight,
    MidLeft,
    MidRight,
    BottomLeft,
    BottomRight,
}

enum Row {
    Top,
    Mid,
    Bottom,
}

impl ToastPos {
    pub const ALL: [Self; 6] = [
        Self::TopLeft,
        Self::TopRight,
        Self::MidLeft,
        Self::MidRight,
        Self::BottomLeft,
        Self::BottomRight,
    ];

    pub fn label(self) -> String {
        t(match self {
            Self::TopLeft => "toast-pos-top-left",
            Self::TopRight => "toast-pos-top-right",
            Self::MidLeft => "toast-pos-mid-left",
            Self::MidRight => "toast-pos-mid-right",
            Self::BottomLeft => "toast-pos-bottom-left",
            Self::BottomRight => "toast-pos-bottom-right",
        })
    }

    fn is_left(self) -> bool {
        matches!(self, Self::TopLeft | Self::MidLeft | Self::BottomLeft)
    }

    fn row(self) -> Row {
        match self {
            Self::TopLeft | Self::TopRight => Row::Top,
            Self::MidLeft | Self::MidRight => Row::Mid,
            Self::BottomLeft | Self::BottomRight => Row::Bottom,
        }
    }
}

/// what a plugin can change on a toast. colors are straight rgba.
#[derive(Debug, Clone, Default)]
pub struct ToastStyle {
    pub accent: Option<[u8; 4]>,
    pub background: Option<[u8; 4]>,
    pub text: Option<[u8; 4]>,
    // seconds on screen, clamped on use
    pub duration: Option<f32>,
    // full path, already checked against the plugin assets folder
    pub image: Option<String>,
}

/// "#rrggbb" or "#rrggbbaa"
pub fn parse_hex(s: &str) -> Option<[u8; 4]> {
    let s = s.trim().trim_start_matches('#');
    if !s.is_ascii() || !(s.len() == 6 || s.len() == 8) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
    Some([
        byte(0)?,
        byte(2)?,
        byte(4)?,
        if s.len() == 8 { byte(6)? } else { 255 },
    ])
}

/// local wall clock
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    min: u8,
    sec: u8,
}

impl Stamp {
    pub fn now() -> Self {
        let st = unsafe { GetLocalTime() };
        Self {
            year: st.wYear,
            month: st.wMonth as u8,
            day: st.wDay as u8,
            hour: st.wHour as u8,
            min: st.wMinute as u8,
            sec: st.wSecond as u8,
        }
    }

    /// date only when it isnt today
    pub fn label(self, today: Stamp) -> String {
        let clock = format!("{:02}:{:02}:{:02}", self.hour, self.min, self.sec);
        if (self.year, self.month, self.day) == (today.year, today.month, today.day) {
            clock
        } else {
            format!("{:02}/{:02} {clock}", self.day, self.month)
        }
    }
}

pub struct PluginTag {
    pub slug: String,
    pub name: String,
}

pub struct Toast {
    pub id: u64,
    pub plugin: Arc<PluginTag>,
    pub text: Box<str>,
    pub at: Stamp,
}

/// cut to the max stored length on a char boundary
pub fn clip_text(s: &str) -> &str {
    s.char_indices().nth(TEXT_MAX).map_or(s, |(i, _)| &s[..i])
}

/// first non empty line, cut short. bool is true when it was cut.
pub fn preview(text: &str) -> (&str, bool) {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let cut = line
        .char_indices()
        .nth(PREVIEW_MAX)
        .map_or(line.len(), |(i, _)| i);
    (&line[..cut], cut < line.len())
}

/// shorten with ... until it fits max px
fn fit_text(s: &str, size: f32, bold: bool, max: f32) -> String {
    if overlay::measure_text(s, size, bold) <= max {
        return s.to_string();
    }
    let ends: Vec<usize> = s.char_indices().map(|(i, _)| i).collect();
    if ends.is_empty() {
        return String::new();
    }
    let (mut lo, mut hi) = (0, ends.len() - 1);
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        let probe = format!("{}...", &s[..ends[mid]]);
        if overlay::measure_text(&probe, size, bold) <= max {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    format!("{}...", &s[..ends[lo]])
}

struct Group {
    tag: Arc<PluginTag>,
    count: u32,
    // only used while count is 1
    preview: String,
    style: ToastStyle,
}

struct Current {
    title: String,
    body: String,
    age: f32,
    secs: f32,
    fit_w: f32,
    title_fit: String,
    body_fit: String,
    accent: [u8; 4],
    bg: [u8; 4],
    text: [u8; 4],
    body_col: [u8; 4],
    image: Option<String>,
}

impl Current {
    fn new(group: Group) -> Self {
        // a group of many shows plain, one toast shows its own style
        let style = if group.count == 1 {
            group.style
        } else {
            ToastStyle::default()
        };
        let text = style.text.unwrap_or(DEF_TEXT);
        let body_col = match style.text {
            Some([r, g, b, a]) => [r, g, b, (a as f32 * 0.8) as u8],
            None => DEF_BODY,
        };
        Self {
            body: if group.count == 1 {
                group.preview
            } else {
                t_args("toast-sent-many", &[("count", group.count.into())])
            },
            title: group.tag.name.clone(),
            age: 0.0,
            secs: style
                .duration
                .filter(|s| s.is_finite())
                .map_or(DEF_SECS, |s| s.clamp(MIN_SECS, MAX_SECS)),
            fit_w: -1.0,
            title_fit: String::new(),
            body_fit: String::new(),
            accent: style.accent.unwrap_or(DEF_ACCENT),
            bg: style.background.unwrap_or(DEF_BG),
            text,
            body_col,
            image: style.image,
        }
    }
}

#[derive(Default)]
pub struct ToastCenter {
    history: VecDeque<Toast>,
    next_id: u64,
    unread: u32,
    tags: Vec<Arc<PluginTag>>,
    waiting: VecDeque<Group>,
    current: Option<Current>,
    last_tick: Option<Instant>,
}

impl ToastCenter {
    pub fn push(&mut self, slug: &str, name: &str, text: &str, style: ToastStyle) {
        let text = clip_text(text.trim_end());
        if text.trim().is_empty() {
            return;
        }
        let tag = self.tag(slug, name);
        if let Some(group) = self.waiting.iter_mut().find(|g| g.tag.slug == slug) {
            group.count += 1;
        } else {
            let (line, cut) = preview(text);
            let preview = if cut { format!("{line}...") } else { line.to_string() };
            self.waiting.push_back(Group {
                tag: Arc::clone(&tag),
                count: 1,
                preview,
                style,
            });
        }
        if self.history.len() >= HISTORY_MAX {
            self.history.pop_front();
        }
        self.history.push_back(Toast {
            id: self.next_id,
            plugin: tag,
            text: text.into(),
            at: Stamp::now(),
        });
        self.next_id += 1;
        self.unread = self.unread.saturating_add(1);
    }

    fn tag(&mut self, slug: &str, name: &str) -> Arc<PluginTag> {
        if let Some(tag) = self.tags.iter_mut().find(|t| t.slug == slug) {
            if tag.name != name {
                *tag = Arc::new(PluginTag {
                    slug: slug.to_string(),
                    name: name.to_string(),
                });
            }
            return Arc::clone(tag);
        }
        let tag = Arc::new(PluginTag {
            slug: slug.to_string(),
            name: name.to_string(),
        });
        self.tags.push(Arc::clone(&tag));
        tag
    }

    /// advance the queue, true when a toast is on screen. time only runs while
    /// the game has focus, so nothing is missed alt tabbed.
    pub fn update(&mut self, game_focused: bool) -> bool {
        let now = Instant::now();
        if !game_focused {
            self.last_tick = None;
            return false;
        }
        let dt = self
            .last_tick
            .replace(now)
            .map_or(0.0, |last| now.duration_since(last).as_secs_f32().min(0.1));
        if let Some(cur) = &mut self.current {
            cur.age += dt;
            if cur.age >= cur.secs {
                self.current = None;
            }
        }
        if self.current.is_none() {
            self.current = self.waiting.pop_front().map(Current::new);
        }
        self.current.is_some()
    }

    /// a toast is on screen right now
    pub fn showing(&self) -> bool {
        self.current.is_some() && self.last_tick.is_some()
    }

    /// something is showing or queued
    pub fn has_work(&self) -> bool {
        self.current.is_some() || !self.waiting.is_empty()
    }

    pub fn draw(&mut self, w: f32, h: f32, pos: ToastPos) {
        let Some(cur) = &mut self.current else {
            return;
        };
        let s = (h / 1080.0).clamp(0.7, 2.5);
        let (cw, ch, margin) = (380.0 * s, 64.0 * s, 24.0 * s);
        let f = if cur.age < FADE_IN {
            cur.age / FADE_IN
        } else if cur.age > cur.secs - FADE_OUT {
            (cur.secs - cur.age) / FADE_OUT
        } else {
            1.0
        }
        .clamp(0.0, 1.0);
        let f = f * f * (3.0 - 2.0 * f);
        let left = pos.is_left();
        let slide = (1.0 - f) * 28.0 * s * if left { -1.0 } else { 1.0 };
        let x = (if left { margin } else { w - margin - cw }) + slide;
        let y = match pos.row() {
            Row::Top => margin,
            Row::Mid => (h - ch) / 2.0,
            Row::Bottom => h - margin - ch,
        };
        let col = |c: [u8; 4]| Rgba(c[0], c[1], c[2], (c[3] as f32 * f) as u8);

        let img = 44.0 * s;
        let tx = x + 24.0 * s + if cur.image.is_some() { img + 12.0 * s } else { 0.0 };
        let inner = x + cw - 14.0 * s - tx;
        if (cur.fit_w - inner).abs() > 0.5 {
            cur.title_fit = fit_text(&cur.title, 15.0 * s, true, inner);
            cur.body_fit = fit_text(&cur.body, 13.0 * s, false, inner);
            cur.fit_w = inner;
        }

        overlay::rect(
            x,
            y,
            cw,
            ch,
            col(cur.bg),
            col([cur.text[0], cur.text[1], cur.text[2], 45]),
            1.0,
            true,
            9.0 * s,
        );
        let accent = col(cur.accent);
        overlay::rect(x + 9.0 * s, y + 12.0 * s, 3.0 * s, ch - 24.0 * s, accent, accent, 0.0, true, 1.5 * s);
        if let Some(path) = &cur.image {
            overlay::image(path, x + 24.0 * s, y + (ch - img) / 2.0, img, img, f, 6.0 * s);
        }
        overlay::text(
            tx,
            y + 10.0 * s,
            &cur.title_fit,
            col(cur.text),
            15.0 * s,
            "",
            "",
            true,
            Some((tx, inner)),
        );
        overlay::text(
            tx,
            y + 33.0 * s,
            &cur.body_fit,
            col(cur.body_col),
            13.0 * s,
            "",
            "",
            false,
            Some((tx, inner)),
        );
    }

    pub fn history(&self) -> impl DoubleEndedIterator<Item = &Toast> {
        self.history.iter()
    }

    pub fn unread(&self) -> u32 {
        self.unread
    }

    pub fn mark_read(&mut self) {
        self.unread = 0;
    }

    /// drops the list, not what is already queued for the overlay
    pub fn clear(&mut self) {
        self.history.clear();
        self.unread = 0;
    }
}
