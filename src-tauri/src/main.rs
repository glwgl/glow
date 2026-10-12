#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tauri::{Emitter, Manager};

#[cfg(windows)]
mod sys;

#[derive(Serialize)]
struct Track { title: String, artist: String, position: f64, duration: f64, playing: bool }
#[derive(Serialize)]
struct Radios { wifi: Option<bool>, bt: Option<bool>, air: Option<bool> }

// Открыт ли сейчас остров (им управляет слежение за мышью)
static OPEN: AtomicBool = AtomicBool::new(false);
// Размер самого острова (логические пиксели): окно больше острова из-за теней и свечения
static ISL_W: AtomicU64 = AtomicU64::new(0);
static ISL_H: AtomicU64 = AtomicU64::new(0);

// ---------- Музыка ----------
#[tauri::command]
async fn now_playing() -> Option<Track> {
    #[cfg(windows)]
    { sys::media::read().ok().map(|t| Track { title: t.title, artist: t.artist, position: t.position, duration: t.duration, playing: t.playing }) }
    #[cfg(not(windows))]
    { None }
}

#[tauri::command]
async fn media_cmd(cmd: String) {
    #[cfg(windows)] { let _ = sys::media::cmd(&cmd); }
    #[cfg(not(windows))] { let _ = cmd; }
}

#[tauri::command]
async fn seek(secs: f64) {
    #[cfg(windows)] { let _ = sys::media::seek(secs); }
    #[cfg(not(windows))] { let _ = secs; }
}

#[tauri::command]
async fn cover(key: String) -> Option<String> {
    #[cfg(windows)] { sys::media::cover(&key) }
    #[cfg(not(windows))] { let _ = key; None }
}

// ---------- Статус ----------
#[tauri::command]
fn battery_info() -> Option<(f32, bool)> {
    let m = battery::Manager::new().ok()?;
    let b = m.batteries().ok()?.next()?.ok()?;
    Some((b.state_of_charge().value * 100.0, b.state() == battery::State::Charging))
}

#[tauri::command]
async fn radios() -> Option<Radios> {
    #[cfg(windows)]
    { sys::radio::get().ok().map(|r| Radios { wifi: r.wifi, bt: r.bt, air: r.air }) }
    #[cfg(not(windows))]
    { None }
}

#[tauri::command]
async fn set_radio(kind: String, on: bool) {
    #[cfg(windows)] { let _ = sys::radio::set(&kind, on); }
    #[cfg(not(windows))] { let _ = (kind, on); }
}

#[tauri::command]
async fn get_volume() -> Option<f32> {
    #[cfg(windows)] { sys::volume::get().ok() }
    #[cfg(not(windows))] { None }
}

#[tauri::command]
async fn set_volume(v: f32) {
    #[cfg(windows)] { let _ = sys::volume::set(v); }
    #[cfg(not(windows))] { let _ = v; }
}

#[tauri::command]
async fn get_brightness() -> Option<u32> {
    #[cfg(windows)] { sys::brightness::get() }
    #[cfg(not(windows))] { None }
}

#[tauri::command]
async fn set_brightness(v: u32) {
    #[cfg(windows)] { let _ = sys::brightness::set(v); }
    #[cfg(not(windows))] { let _ = v; }
}

// Открывает страницу «Параметры Windows» (ms-settings:...)
#[tauri::command]
fn open_settings(uri: String) {
    #[cfg(windows)] { sys::open_settings(&uri); }
    #[cfg(not(windows))] { let _ = uri; }
}

// ---------- Окно ----------
// Подгоняет окно под нужный размер, центрирует вверху экрана и показывает.
// iw, ih: размер самого острова. through: окно пропускает клики (когда остров закрыт).
#[tauri::command]
fn fit(win: tauri::WebviewWindow, w: f64, h: f64, iw: f64, ih: f64, through: bool) {
    ISL_W.store(iw.to_bits(), Ordering::Relaxed);
    ISL_H.store(ih.to_bits(), Ordering::Relaxed);
    let _ = win.set_size(tauri::LogicalSize::new(w, h));
    if let Ok(Some(m)) = win.primary_monitor() {
        let sf = m.scale_factor();
        let x = m.position().x as f64 / sf + (m.size().width as f64 / sf - w) / 2.0;
        let y = m.position().y as f64 / sf;
        let _ = win.set_position(tauri::LogicalPosition::new(x, y));
    }
    let _ = win.set_ignore_cursor_events(through);
    let _ = win.show();
}

#[tauri::command]
fn hide_window(win: tauri::WebviewWindow) { let _ = win.hide(); }

// Интерфейс сообщает, что остров открыт или закрыт (например, открыли кликом)
#[tauri::command]
fn set_open(v: bool) { OPEN.store(v, Ordering::Relaxed); }

#[tauri::command]
fn quit(app: tauri::AppHandle) { app.exit(0); }

// Следит за мышью: у самого верха экрана по центру остров открывается,
// когда мышь уходит с острова, он закрывается сам
fn watch_cursor(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let (mut outside, mut since_open) = (0u32, 0u32);
        loop {
            std::thread::sleep(std::time::Duration::from_millis(40));
            let Some(win) = app.get_webview_window("main") else { continue };
            let (Ok(c), Ok(Some(m))) = (win.cursor_position(), win.primary_monitor()) else { continue };
            let sf = m.scale_factor();
            let (mx, my) = (m.position().x as f64, m.position().y as f64);
            let mw = m.size().width as f64;
            if !OPEN.load(Ordering::Relaxed) {
                let hot = c.y >= my && c.y - my <= 3.0 && (c.x - (mx + mw / 2.0)).abs() <= 150.0 * sf;
                if hot {
                    OPEN.store(true, Ordering::Relaxed);
                    (outside, since_open) = (0, 0);
                    let _ = app.emit("island-open", ());
                }
            } else {
                since_open += 1;
                let (Ok(p), Ok(s)) = (win.outer_position(), win.outer_size()) else { continue };
                let (iw, ih) = (f64::from_bits(ISL_W.load(Ordering::Relaxed)) * sf, f64::from_bits(ISL_H.load(Ordering::Relaxed)) * sf);
                let cx = p.x as f64 + s.width as f64 / 2.0;
                let top = p.y as f64 + 12.0 * sf;
                let pad = 10.0 * sf;
                let inside = c.x >= cx - iw / 2.0 - pad
                    && c.x <= cx + iw / 2.0 + pad
                    && c.y >= top - 14.0 * sf
                    && c.y <= top + ih + pad;
                if inside { outside = 0 } else { outside += 1 }
                // около 0,3 с вне острова, и не раньше чем через 0,6 с после открытия
                if outside >= 8 && since_open > 15 {
                    OPEN.store(false, Ordering::Relaxed);
                    let _ = app.emit("island-close", ());
                }
            }
        }
    });
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            now_playing, media_cmd, seek, cover, battery_info, radios, set_radio,
            get_volume, set_volume, get_brightness, set_brightness, open_settings,
            fit, hide_window, set_open, quit
        ])
        .setup(|app| { watch_cursor(app.handle().clone()); Ok(()) })
        .run(tauri::generate_context!())
        .expect("не удалось запустить Glow");
}
