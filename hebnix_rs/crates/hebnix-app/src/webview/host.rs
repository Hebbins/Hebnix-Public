//! the browser the overlay window hosts.
//!
//! one for all of hebnix, plugin pages are iframes in its shell page.
//!
//! creation is async and never pumped by hand: wait_with_pump would reenter
//! egui, tick() picks the results up on later frames instead.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_BOUNDS_MODE_USE_RAW_PIXELS, COREWEBVIEW2_COLOR,
    COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_DENY_CORS,
    COREWEBVIEW2_MOUSE_EVENT_KIND_HORIZONTAL_WHEEL,
    COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOUBLE_CLICK,
    COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOWN, COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_UP,
    COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOUBLE_CLICK,
    COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOWN,
    COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_UP, COREWEBVIEW2_MOUSE_EVENT_KIND_MOVE,
    COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOUBLE_CLICK,
    COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOWN, COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_UP,
    COREWEBVIEW2_MOUSE_EVENT_KIND_WHEEL, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS,
    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_IMAGE, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MEDIA,
    COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_ALL, CreateCoreWebView2EnvironmentWithOptions,
    ICoreWebView2, ICoreWebView2_3, ICoreWebView2_22, ICoreWebView2CompositionController,
    ICoreWebView2Controller, ICoreWebView2Controller2, ICoreWebView2Controller3,
    ICoreWebView2Environment, ICoreWebView2Environment3, ICoreWebView2WebResourceResponse,
};
use webview2_com::{
    AddScriptToExecuteOnDocumentCreatedCompletedHandler,
    CreateCoreWebView2CompositionControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler, WebMessageReceivedEventHandler,
};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::DirectComposition::IDCompositionVisual;
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Com::StructuredStorage::CreateStreamOnHGlobal;
use windows::core::{Interface, PCWSTR};

/// hebnix's own page, plugin pages are iframes inside #layers
const SHELL_HTML: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><style>
  html,body { margin:0; height:100%; background:transparent; overflow:hidden; }
  #layers { position:absolute; inset:0; }
  /* dom order is stacking order, so a canvas and an iframe interleave freely */
  #layers > iframe, #layers > canvas {
    position:absolute; inset:0; width:100%; height:100%;
    border:0; background:transparent; pointer-events:none;
  }
  #layers > iframe[data-clickable="true"] { pointer-events:auto; }
</style>
<script>
window.__hebnix = {
  setLayers: function (specs) {
    var host = document.getElementById('layers');
    window.chrome.webview.postMessage({kind:'pointer-hit', hit:false});
    host.textContent = '';
    specs.forEach(function (spec) {
      var el;
      if (spec.url) {
        el = document.createElement('iframe');
        el.setAttribute('scrolling', 'no');
        el.dataset.clickable = spec.clickable ? 'true' : 'false';
        el.src = spec.url;
      } else {
        el = document.createElement('canvas');
      }
      el.dataset.slug = spec.slug;
      host.appendChild(el);
    });
  },
  deliver: function (slug, data) {
    var frame = document.querySelector('#layers > iframe[data-slug="' + slug + '"]');
    if (frame && frame.contentWindow) {
      frame.contentWindow.postMessage(data, '*');
    }
  },
  // one message per frame for every plugin, a call per primitive would be
  // thousands a second
  paint: function (batches, w, h) {
    batches.forEach(function (batch) {
      var canvas = document.querySelector('#layers > canvas[data-slug="' + batch.slug + '"]');
      if (!canvas) return;
      var ctx = canvas.getContext('2d');
      // physical pixels, not clientWidth. css pixels differ under scaling.
      if (canvas.width !== w || canvas.height !== h) { canvas.width = w; canvas.height = h; }
      ctx.clearRect(0, 0, w, h);
      var base = batch.assets;
      batch.ops.forEach(function (o) { window.__hebnix.op(ctx, o, base); });
    });
  },
  images: {},
  op: function (ctx, o, base) {
    ctx.save();
    switch (o.op) {
      case 'line':
        ctx.strokeStyle = o.color; ctx.lineWidth = o.width;
        ctx.beginPath(); ctx.moveTo(o.x1, o.y1); ctx.lineTo(o.x2, o.y2); ctx.stroke();
        break;
      case 'rect':
        ctx.beginPath();
        if (o.radius > 0 && ctx.roundRect) ctx.roundRect(o.x, o.y, o.w, o.h, o.radius);
        else ctx.rect(o.x, o.y, o.w, o.h);
        if (o.filled) { ctx.fillStyle = o.fill; ctx.fill(); }
        if (o.width > 0) { ctx.strokeStyle = o.border; ctx.lineWidth = o.width; ctx.stroke(); }
        break;
      case 'circle':
        ctx.beginPath(); ctx.arc(o.x, o.y, o.r, 0, Math.PI * 2);
        if (o.filled) { ctx.fillStyle = o.color; ctx.fill(); }
        else { ctx.strokeStyle = o.color; ctx.lineWidth = o.width; ctx.stroke(); }
        break;
      case 'text':
        ctx.fillStyle = o.color;
        ctx.font = o.size + 'px Segoe UI, sans-serif';
        ctx.textAlign = o.halign === 'center' ? 'center' : (o.halign === 'right' ? 'right' : 'left');
        ctx.textBaseline = 'top';
        ctx.fillText(o.text, o.x, o.y);
        break;
      case 'polygon':
        ctx.fillStyle = o.color; ctx.beginPath();
        o.points.forEach(function (p, i) { i ? ctx.lineTo(p[0], p[1]) : ctx.moveTo(p[0], p[1]); });
        ctx.closePath(); ctx.fill();
        break;
      case 'image': {
        // a fresh Image() every frame would refetch forever
        var url = base + '/' + String(o.path).replace(/^\/+/, '').replace(/^assets\//, '');
        var img = window.__hebnix.images[url];
        if (!img) { img = new Image(); img.src = url; window.__hebnix.images[url] = img; }
        if (img.complete && img.naturalWidth) {
          ctx.globalAlpha = o.opacity;
          ctx.drawImage(img, o.x, o.y, o.w, o.h);
        }
        break;
      }
    }
    ctx.restore();
  },
};
window.addEventListener('message', function (event) {
  var data = event.data;
  if (!data || data.__hebnixPointerHit !== true) return;
  var frames = Array.from(document.querySelectorAll('#layers > iframe[data-clickable="true"]'));
  var frame = frames.find(function (candidate) { return candidate.contentWindow === event.source; });
  if (!frame) return;
  window.chrome.webview.postMessage({kind:'pointer-hit', hit:data.hit === true, region:data.region || null});
});
window.chrome.webview.addEventListener('message', function (event) {
  var msg = event.data;
  if (!msg || !msg.kind) return;
  if (msg.kind === 'layers') window.__hebnix.setLayers(msg.layers);
  else if (msg.kind === 'paint') window.__hebnix.paint(msg.batches, msg.w, msg.h);
  else if (msg.kind === 'deliver') window.__hebnix.deliver(msg.slug, msg.data);
});
</script></head>
<body><div id="layers"></div></body></html>"#;

const POINTER_HIT_SCRIPT: &str = r#"
(function () {
  if (window === window.top || window.__hebnixPointerHitInstalled) return;
  window.__hebnixPointerHitInstalled = true;

  function visibleColor(value) {
    if (!value || value === 'transparent') return false;
    var match = value.match(/^rgba\([^,]+,[^,]+,[^,]+,\s*([\d.]+)\)$/i);
    return !match || Number(match[1]) > 0;
  }

  function isPainted(element) {
    var style = getComputedStyle(element);
    if (style.display === 'none' || style.visibility === 'hidden' || Number(style.opacity) <= 0) return false;
    var rect = element.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return false;
    var tag = element.tagName.toLowerCase();
    if (/^(a|button|input|select|textarea|summary|img|svg|canvas|video)$/.test(tag)) return true;
    if (element.isContentEditable || element.onclick || element.getAttribute('role') === 'button') return true;
    if (visibleColor(style.backgroundColor) || style.backgroundImage !== 'none') return true;
    if (style.boxShadow !== 'none' || style.outlineStyle !== 'none') return true;
    var sides = ['Top', 'Right', 'Bottom', 'Left'];
    if (sides.some(function (side) {
      return parseFloat(style['border' + side + 'Width']) > 0 &&
        style['border' + side + 'Style'] !== 'none' &&
        visibleColor(style['border' + side + 'Color']);
    })) return true;
    var overflow = style.overflow + ' ' + style.overflowX + ' ' + style.overflowY;
    return /(auto|scroll)/.test(overflow) &&
      (element.scrollHeight > element.clientHeight || element.scrollWidth > element.clientWidth);
  }

  function report(event) {
    var painted = document.elementsFromPoint(event.clientX, event.clientY).filter(isPainted);
    var region = null;
    painted.forEach(function (element) {
      var rect = element.getBoundingClientRect();
      if (!region || rect.width * rect.height > region.width * region.height) {
        region = {left:rect.left, top:rect.top, right:rect.right, bottom:rect.bottom, width:rect.width, height:rect.height};
      }
    });
    window.parent.postMessage({
      __hebnixPointerHit:true,
      hit:painted.length > 0,
      region:region ? {left:region.left, top:region.top, right:region.right, bottom:region.bottom} : null
    }, '*');
  }

  var layoutFrame = 0;
  function reportLayout() {
    if (layoutFrame) return;
    layoutFrame = requestAnimationFrame(function () {
      layoutFrame = 0;
      var region = null;
      var elements = document.body ? document.body.querySelectorAll('*') : [];
      Array.prototype.forEach.call(elements, function (element) {
        if (!isPainted(element)) return;
        var rect = element.getBoundingClientRect();
        var left = Math.max(0, rect.left);
        var top = Math.max(0, rect.top);
        var right = Math.min(window.innerWidth, rect.right);
        var bottom = Math.min(window.innerHeight, rect.bottom);
        var area = Math.max(0, right - left) * Math.max(0, bottom - top);
        if (area > 0 && (!region || area > region.area)) {
          region = {left:left, top:top, right:right, bottom:bottom, area:area};
        }
      });
      window.parent.postMessage({
        __hebnixPointerHit:true,
        hit:false,
        region:region ? {left:region.left, top:region.top, right:region.right, bottom:region.bottom} : null
      }, '*');
    });
  }

  document.addEventListener('mousemove', report, true);
  document.addEventListener('transitionend', reportLayout, true);
  document.addEventListener('animationend', reportLayout, true);
  window.addEventListener('load', reportLayout);
  window.addEventListener('resize', reportLayout);
  function startLayoutWatch() {
    var root = document.documentElement;
    if (!root) return;
    new MutationObserver(reportLayout).observe(root, {
      attributes:true,
      childList:true,
      subtree:true
    });
    if (window.ResizeObserver) new ResizeObserver(reportLayout).observe(root);
    reportLayout();
    setTimeout(reportLayout, 50);
    setTimeout(reportLayout, 300);
  }
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', startLayoutWatch, {once:true});
  } else {
    startLayoutWatch();
  }
})();
"#;

/// beside the extracted curl-impersonate, the install dir is not always writable
fn user_data_dir() -> Option<std::path::PathBuf> {
    let dir = dirs::data_local_dir()?.join("Hebnix").join("webview2");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

#[derive(Clone, PartialEq, Eq)]
struct PageSpec {
    slug: String,
    host: String,
    /// None for draw-only, it still needs the mapping for draw.image
    page: Option<String>,
    assets: std::path::PathBuf,
    clickable: bool,
}

/// a slug can hold anything a folder name allows, a hostname cannot
fn virtual_host(slug: &str) -> String {
    let cleaned: String = slug
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('-').to_string();
    if cleaned.is_empty() {
        "plugin.hebnix".to_string()
    } else {
        format!("{cleaned}.plugin.hebnix")
    }
}

enum Stage {
    Environment,
    HaveEnvironment(ICoreWebView2Environment),
    Controller,
    Ready(Ready),
    Failed(String),
}

static POINTER_HIT: AtomicBool = AtomicBool::new(false);
static POINTER_REGION_VALID: AtomicBool = AtomicBool::new(false);
static POINTER_REGION_LEFT: AtomicI32 = AtomicI32::new(0);
static POINTER_REGION_TOP: AtomicI32 = AtomicI32::new(0);
static POINTER_REGION_RIGHT: AtomicI32 = AtomicI32::new(0);
static POINTER_REGION_BOTTOM: AtomicI32 = AtomicI32::new(0);

thread_local! {
    static INPUT_CONTROLLER: RefCell<Option<ICoreWebView2CompositionController>> = const { RefCell::new(None) };
}

pub fn pointer_hit_at(hwnd: HWND, mut point: POINT) -> bool {
    if POINTER_HIT.load(Ordering::Relaxed) {
        return true;
    }
    if !POINTER_REGION_VALID.load(Ordering::Relaxed) {
        return false;
    }
    unsafe {
        let _ = ScreenToClient(hwnd, &mut point);
    }
    point.x >= POINTER_REGION_LEFT.load(Ordering::Relaxed)
        && point.x < POINTER_REGION_RIGHT.load(Ordering::Relaxed)
        && point.y >= POINTER_REGION_TOP.load(Ordering::Relaxed)
        && point.y < POINTER_REGION_BOTTOM.load(Ordering::Relaxed)
}

pub fn clear_pointer_hit() {
    POINTER_HIT.store(false, Ordering::Relaxed);
    POINTER_REGION_VALID.store(false, Ordering::Relaxed);
}

pub fn forward_mouse_screen_message(
    hwnd: HWND,
    msg: u32,
    mut point: POINT,
    mouse_data: u32,
) -> bool {
    unsafe {
        let _ = ScreenToClient(hwnd, &mut point);
    }
    forward_mouse_at_point(msg, 0, point, mouse_data)
}

fn forward_mouse_at_point(msg: u32, key_state: u32, point: POINT, mouse_data: u32) -> bool {
    let kind = match msg {
        0x0200 => COREWEBVIEW2_MOUSE_EVENT_KIND_MOVE,
        0x0201 => COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOWN,
        0x0202 => COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_UP,
        0x0203 => COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOUBLE_CLICK,
        0x0204 => COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOWN,
        0x0205 => COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_UP,
        0x0206 => COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOUBLE_CLICK,
        0x0207 => COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOWN,
        0x0208 => COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_UP,
        0x0209 => COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOUBLE_CLICK,
        0x020A => COREWEBVIEW2_MOUSE_EVENT_KIND_WHEEL,
        0x020E => COREWEBVIEW2_MOUSE_EVENT_KIND_HORIZONTAL_WHEEL,
        _ => return false,
    };
    let wheel_data = if msg == 0x020A || msg == 0x020E {
        (((mouse_data >> 16) as u16 as i16) as i32) as u32
    } else {
        0
    };
    let keys = COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS(key_state as i32);
    INPUT_CONTROLLER.with(|slot| {
        slot.borrow().as_ref().is_some_and(|controller| unsafe {
            controller
                .SendMouseInput(kind, keys, wheel_data, point)
                .is_ok()
        })
    })
}

struct Ready {
    #[allow(dead_code)]
    composition: ICoreWebView2CompositionController,
    controller: ICoreWebView2Controller,
    #[allow(dead_code)]
    webview: ICoreWebView2,
}

pub struct WebviewHost {
    stage: Rc<RefCell<Stage>>,
    visual: IDCompositionVisual,
    bounds: Option<(u32, u32)>,
    visible: bool,
    shown: Option<(u32, u32)>,
    logged_ready: bool,
    pages: Vec<PageSpec>,
}

impl WebviewHost {
    pub fn new(visual: IDCompositionVisual) -> Self {
        let stage = Rc::new(RefCell::new(Stage::Environment));
        let sink = Rc::clone(&stage);
        let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(
            move |result, environment| {
                *sink.borrow_mut() = match (result, environment) {
                    (Ok(()), Some(environment)) => Stage::HaveEnvironment(environment),
                    (Err(error), _) => Stage::Failed(error.message()),
                    (Ok(()), None) => Stage::Failed("no environment came back".to_string()),
                };
                Ok(())
            },
        ));
        let data_dir: Vec<u16> = match user_data_dir() {
            Some(dir) => format!("{}\0", dir.display()).encode_utf16().collect(),
            None => vec![0],
        };
        let created = unsafe {
            CreateCoreWebView2EnvironmentWithOptions(
                PCWSTR::null(),
                PCWSTR(data_dir.as_ptr()),
                None,
                &handler,
            )
        };
        if let Err(error) = created {
            *stage.borrow_mut() = Stage::Failed(error.message());
        }
        Self {
            stage,
            visual,
            bounds: None,
            visible: false,
            shown: None,
            logged_ready: false,
            pages: Vec::new(),
        }
    }

    pub fn take_error(&mut self) -> Option<String> {
        let mut stage = self.stage.borrow_mut();
        if let Stage::Failed(message) = &*stage {
            let message = message.clone();
            *stage = Stage::Failed(String::new());
            return (!message.is_empty()).then_some(message);
        }
        None
    }

    #[allow(dead_code)]
    pub fn is_ready(&self) -> bool {
        matches!(&*self.stage.borrow(), Stage::Ready(_))
    }

    pub fn wants_overlay(&self) -> bool {
        self.pages.iter().any(|spec| spec.page.is_some()) && self.is_ready()
    }

    pub fn wants_input(&self) -> bool {
        self.pages
            .iter()
            .any(|spec| spec.page.is_some() && spec.clickable)
            && self.is_ready()
    }

    pub fn sync_pages(&mut self, pages: &[(String, Option<String>, std::path::PathBuf, bool)]) {
        let wanted: Vec<PageSpec> = pages
            .iter()
            .map(|(slug, page, assets, clickable)| PageSpec {
                slug: slug.clone(),
                host: virtual_host(slug),
                page: page.clone(),
                assets: assets.clone(),
                clickable: *clickable,
            })
            .collect();
        if wanted == self.pages {
            return;
        }

        let stage = self.stage.borrow();
        let Stage::Ready(ready) = &*stage else {
            return; // retried next frame, sync_pages runs from the frame loop
        };
        let Ok(webview) = ready.webview.cast::<ICoreWebView2_3>() else {
            drop(stage);
            self.pages = wanted;
            tracing::warn!("runtime has no virtual host mapping, pages are off");
            return;
        };

        unsafe {
            for spec in &self.pages {
                let host: Vec<u16> = format!("{}\0", spec.host).encode_utf16().collect();
                let _ = webview.ClearVirtualHostNameToFolderMapping(PCWSTR(host.as_ptr()));
            }
            for spec in &wanted {
                let host: Vec<u16> = format!("{}\0", spec.host).encode_utf16().collect();
                let folder: Vec<u16> = format!("{}\0", spec.assets.display())
                    .encode_utf16()
                    .collect();
                if let Err(error) = webview.SetVirtualHostNameToFolderMapping(
                    PCWSTR(host.as_ptr()),
                    PCWSTR(folder.as_ptr()),
                    COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_DENY_CORS,
                ) {
                    tracing::warn!("mapping {} failed: {}", spec.host, error.message());
                }
            }
        }
        drop(stage);

        // page to iframe, draw plugin to canvas. dom order is stacking order.
        let layers: Vec<serde_json::Value> = wanted
            .iter()
            .map(|spec| match spec.page.as_deref() {
                Some(page) => serde_json::json!({
                    "slug": spec.slug,
                    "url": format!("https://{}/{}", spec.host, page),
                    "clickable": spec.clickable,
                }),
                None => serde_json::json!({ "slug": spec.slug }),
            })
            .collect();
        let message = serde_json::json!({ "kind": "layers", "layers": layers }).to_string();
        if let Err(error) = self.post_json(&message) {
            tracing::warn!("pushing overlay layers failed: {error}");
        }
        tracing::info!("overlay layers: {}", layers.len());
        self.pages = wanted;
    }

    pub fn deliver(&self, slug: &str, data: serde_json::Value) -> Result<(), String> {
        if !self.pages.iter().any(|spec| spec.slug == slug) {
            return Err("this plugin has no overlay page".to_string());
        }
        let message =
            serde_json::json!({ "kind": "deliver", "slug": slug, "data": data }).to_string();
        self.post_json(&message)
    }

    fn post_json(&self, message: &str) -> Result<(), String> {
        let stage = self.stage.borrow();
        let Stage::Ready(ready) = &*stage else {
            return Err("the webview is not ready".to_string());
        };
        let wide: Vec<u16> = format!("{message}\0").encode_utf16().collect();
        unsafe {
            ready
                .webview
                .PostWebMessageAsJson(PCWSTR(wide.as_ptr()))
                .map_err(|error| error.message().to_string())
        }
    }

    pub fn status(&self) -> String {
        match &*self.stage.borrow() {
            Stage::Environment => "creating the environment".to_string(),
            Stage::HaveEnvironment(_) => "environment ready, asking for a controller".to_string(),
            Stage::Controller => "creating the controller".to_string(),
            Stage::Ready(_) => match (self.bounds, self.shown) {
                (Some((w, h)), shown) => format!(
                    "ready, {w}x{h}, visible={}, last shown {}",
                    self.visible,
                    match shown {
                        Some((sw, sh)) => format!("{sw}x{sh}"),
                        None => "never".to_string(),
                    }
                ),
                (None, _) => "ready, no bounds yet".to_string(),
            },
            Stage::Failed(message) if message.is_empty() => "failed (already reported)".to_string(),
            Stage::Failed(message) => format!("failed: {message}"),
        }
    }

    /// every frame, visible=false when the overlay is down
    pub fn tick(&mut self, hwnd: HWND, size: (u32, u32), visible: bool) {
        let next = match &*self.stage.borrow() {
            Stage::HaveEnvironment(environment) => Some(environment.clone()),
            _ => None,
        };
        if let Some(environment) = next {
            self.request_controller(hwnd, environment);
        }

        let stage = self.stage.borrow();
        let Stage::Ready(ready) = &*stage else {
            return;
        };
        if !self.logged_ready {
            self.logged_ready = true;
            tracing::info!("overlay webview ready");
        }
        unsafe {
            if self.bounds != Some(size) {
                let _ = ready.controller.SetBounds(RECT {
                    left: 0,
                    top: 0,
                    right: size.0 as i32,
                    bottom: size.1 as i32,
                });
                self.bounds = Some(size);
            }
            if self.visible != visible {
                let _ = ready.controller.SetIsVisible(visible);
                self.visible = visible;
                if !visible {
                    clear_pointer_hit();
                }
                if visible {
                    self.shown = Some(size);
                    tracing::info!("overlay webview shown at {}x{}", size.0, size.1);
                }
            }
        }
    }

    fn request_controller(&mut self, hwnd: HWND, environment: ICoreWebView2Environment) {
        let Ok(env3) = environment.cast::<ICoreWebView2Environment3>() else {
            *self.stage.borrow_mut() =
                Stage::Failed("this runtime has no composition support".to_string());
            return;
        };
        *self.stage.borrow_mut() = Stage::Controller;

        let sink = Rc::clone(&self.stage);
        let visual = self.visual.clone();
        let handler = CreateCoreWebView2CompositionControllerCompletedHandler::create(Box::new(
            move |result, composition| {
                let outcome = match (result, composition) {
                    (Ok(()), Some(composition)) => {
                        finish(composition, &visual, environment.clone())
                    }
                    (Err(error), _) => Err(error.message()),
                    (Ok(()), None) => Err("no controller came back".to_string()),
                };
                *sink.borrow_mut() = match outcome {
                    Ok(ready) => Stage::Ready(ready),
                    Err(message) => Stage::Failed(message),
                };
                Ok(())
            },
        ));
        if let Err(error) = unsafe { env3.CreateCoreWebView2CompositionController(hwnd, &handler) }
        {
            *self.stage.borrow_mut() = Stage::Failed(error.message());
        }
    }
}

/// pictures, audio and video only from the plugin's own folder or an avatar
/// hebnix already cached, so a page cannot phone home through an img tag.
/// scripts and fonts are not filtered, plugin review covers those.
fn install_media_gate(webview: &ICoreWebView2, environment: &ICoreWebView2Environment) {
    let all: Vec<u16> = "*\0".encode_utf16().collect();
    let Ok(filtered) = webview.cast::<ICoreWebView2_22>() else {
        tracing::warn!("runtime is too old to filter iframe media, pages are unrestricted");
        return;
    };
    unsafe {
        for context in [
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_IMAGE,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MEDIA,
        ] {
            if let Err(error) = filtered.AddWebResourceRequestedFilterWithRequestSourceKinds(
                PCWSTR(all.as_ptr()),
                context,
                COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_ALL,
            ) {
                tracing::warn!("media gate filter failed: {}", error.message());
                return;
            }
        }
    }

    let environment = environment.clone();
    let handler =
        webview2_com::WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
            let Some(args) = args else {
                return Ok(());
            };
            unsafe {
                let request = args.Request()?;
                let mut raw = windows::core::PWSTR::null();
                request.Uri(&mut raw)?;
                let uri = webview2_com::take_pwstr(raw);
                if crate::overlay::media_host_allowed(&uri) {
                    return Ok(());
                }
                // same rule as ui.image, only what hebnix already fetched
                if let Some(bytes) = crate::plugins::lua_api::tracker_client().avatar_bytes(&uri) {
                    if let Ok(response) = cached_response(&environment, &bytes) {
                        args.SetResponse(&response)?;
                        return Ok(());
                    }
                }
                tracing::info!("overlay blocked remote media: {uri}");
                if let Ok(response) = blocked_response(&environment) {
                    args.SetResponse(&response)?;
                }
            }
            Ok(())
        }));
    let mut token = 0i64;
    unsafe {
        if let Err(error) = webview.add_WebResourceRequested(&handler, &mut token) {
            tracing::warn!("media gate handler failed: {}", error.message());
        }
    }
}

fn cached_response(
    environment: &ICoreWebView2Environment,
    bytes: &[u8],
) -> windows::core::Result<ICoreWebView2WebResourceResponse> {
    let stream = unsafe {
        let stream = CreateStreamOnHGlobal(windows::Win32::Foundation::HGLOBAL::default(), true)?;
        // a short write would serve a truncated image
        let mut written = 0u32;
        stream
            .Write(
                bytes.as_ptr() as *const _,
                bytes.len() as u32,
                Some(&mut written),
            )
            .ok()?;
        if written as usize != bytes.len() {
            return Err(windows::core::Error::from_hresult(
                windows::Win32::Foundation::E_FAIL,
            ));
        }
        stream.Seek(0, windows::Win32::System::Com::STREAM_SEEK_SET, None)?;
        stream
    };
    let reason: Vec<u16> = "OK\0".encode_utf16().collect();
    let headers: Vec<u16> = "Content-Type: image/*\0".encode_utf16().collect();
    unsafe {
        environment.CreateWebResourceResponse(
            &stream,
            200,
            PCWSTR(reason.as_ptr()),
            PCWSTR(headers.as_ptr()),
        )
    }
}

fn blocked_response(
    environment: &ICoreWebView2Environment,
) -> windows::core::Result<ICoreWebView2WebResourceResponse> {
    let reason: Vec<u16> = "Blocked by Hebnix\0".encode_utf16().collect();
    let headers: Vec<u16> = "\0".encode_utf16().collect();
    unsafe {
        environment.CreateWebResourceResponse(
            None,
            403,
            PCWSTR(reason.as_ptr()),
            PCWSTR(headers.as_ptr()),
        )
    }
}

fn install_pointer_hit_test(webview: &ICoreWebView2) -> Result<(), String> {
    let received = WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
        if let Some(args) = args {
            let mut raw = windows::core::PWSTR::null();
            if unsafe { args.WebMessageAsJson(&mut raw) }.is_ok() {
                let message = webview2_com::take_pwstr(raw);
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&message)
                    && value.get("kind").and_then(|kind| kind.as_str()) == Some("pointer-hit")
                {
                    POINTER_HIT.store(
                        value
                            .get("hit")
                            .and_then(|hit| hit.as_bool())
                            .unwrap_or(false),
                        Ordering::Relaxed,
                    );
                    if let Some(region) = value.get("region").and_then(|region| region.as_object())
                    {
                        let coordinate = |name: &str| {
                            region
                                .get(name)
                                .and_then(|value| value.as_f64())
                                .map(|value| value.round() as i32)
                        };
                        if let (Some(left), Some(top), Some(right), Some(bottom)) = (
                            coordinate("left"),
                            coordinate("top"),
                            coordinate("right"),
                            coordinate("bottom"),
                        ) {
                            POINTER_REGION_LEFT.store(left, Ordering::Relaxed);
                            POINTER_REGION_TOP.store(top, Ordering::Relaxed);
                            POINTER_REGION_RIGHT.store(right, Ordering::Relaxed);
                            POINTER_REGION_BOTTOM.store(bottom, Ordering::Relaxed);
                            POINTER_REGION_VALID
                                .store(right > left && bottom > top, Ordering::Relaxed);
                        }
                    } else if !POINTER_HIT.load(Ordering::Relaxed) {
                        POINTER_REGION_VALID.store(false, Ordering::Relaxed);
                    }
                }
            }
        }
        Ok(())
    }));
    let mut token = 0i64;
    unsafe {
        webview
            .add_WebMessageReceived(&received, &mut token)
            .map_err(|error| format!("add_WebMessageReceived: {}", error.message()))?;
    }

    let completed =
        AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(|result, _| {
            result?;
            Ok(())
        }));
    let script: Vec<u16> = format!("{POINTER_HIT_SCRIPT}\0").encode_utf16().collect();
    unsafe {
        webview
            .AddScriptToExecuteOnDocumentCreated(PCWSTR(script.as_ptr()), &completed)
            .map_err(|error| format!("AddScriptToExecuteOnDocumentCreated: {}", error.message()))?;
    }
    Ok(())
}

fn finish(
    composition: ICoreWebView2CompositionController,
    visual: &IDCompositionVisual,
    environment: ICoreWebView2Environment,
) -> Result<Ready, String> {
    unsafe {
        INPUT_CONTROLLER.with(|slot| *slot.borrow_mut() = Some(composition.clone()));
        composition
            .SetRootVisualTarget(visual)
            .map_err(|error| format!("SetRootVisualTarget: {}", error.message()))?;
        let controller: ICoreWebView2Controller = composition
            .cast()
            .map_err(|error| error.message().to_string())?;
        if let Ok(controller3) = controller.cast::<ICoreWebView2Controller3>() {
            let _ = controller3.SetShouldDetectMonitorScaleChanges(false);
            let _ = controller3.SetRasterizationScale(1.0);
            let _ = controller3.SetBoundsMode(COREWEBVIEW2_BOUNDS_MODE_USE_RAW_PIXELS);
        }
        // without zero alpha the page ships an opaque sheet over the game
        if let Ok(controller2) = controller.cast::<ICoreWebView2Controller2>() {
            let _ = controller2.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR {
                A: 0,
                R: 0,
                G: 0,
                B: 0,
            });
        }
        let webview = controller
            .CoreWebView2()
            .map_err(|error| error.message().to_string())?;
        // before the first navigation, or the shell's loads race it
        install_media_gate(&webview, &environment);
        install_pointer_hit_test(&webview)?;
        let html: Vec<u16> = format!("{SHELL_HTML}\0").encode_utf16().collect();
        webview
            .NavigateToString(PCWSTR(html.as_ptr()))
            .map_err(|error| format!("NavigateToString: {}", error.message()))?;
        Ok(Ready {
            composition,
            controller,
            webview,
        })
    }
}
