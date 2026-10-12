#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
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
// Пауза: остров не открывается при наведении (меню в трее)
static PAUSED: AtomicBool = AtomicBool::new(false);
// Запрос на открытие или закрытие по горячей клавише
static FORCE: AtomicBool = AtomicBool::new(false);
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

#[derive(Serialize)]
struct Names { wifi: Option<String>, bt: Vec<String> }

// К какому Wi-Fi и каким Bluetooth-устройствам подключён компьютер
#[tauri::command]
async fn net_names() -> Names {
    #[cfg(windows)]
    { Names { wifi: sys::names::wifi(), bt: sys::names::bluetooth() } }
    #[cfg(not(windows))]
    { Names { wifi: None, bt: vec![] } }
}

// Открывает страницу «Параметры Windows» (ms-settings:...)
#[tauri::command]
fn open_settings(uri: String) {
    #[cfg(windows)] { sys::open_settings(&uri); }
    #[cfg(not(windows))] { let _ = uri; }
}

// ---------- Окно ----------
// Окно всегда одного размера: оно никогда не меняет размер и положение, поэтому
// остров не дёргается и не лагает. Когда остров закрыт, окно пропускает клики.
const WIN_W: f64 = 700.0;
const WIN_H: f64 = 520.0;

// Форма острова в координатах окна: по ней обрезается размытая подложка.
// tw, th: размер, до которого остров вырастет (для слежения за мышью).
#[tauri::command]
fn shape(app: tauri::AppHandle, x: f64, y: f64, w: f64, h: f64, r: f64, tw: f64, th: f64, on: bool) {
    ISL_W.store(tw.to_bits(), Ordering::Relaxed);
    ISL_H.store(th.to_bits(), Ordering::Relaxed);
    #[cfg(windows)]
    {
        if let Some(b) = app.get_webview_window("blur") {
            if let Ok(hw) = b.hwnd() {
                let sf = b.scale_factor().unwrap_or(1.0);
                sys::blur::set_region(hw.0 as isize, (x * sf).round() as i32, (y * sf).round() as i32,
                                      (w * sf).round() as i32, (h * sf).round() as i32, (r * sf).round() as i32, on);
            }
        }
    }
    #[cfg(not(windows))]
    { let _ = (app, x, y, w, h, r, on); }
}

// Интерфейс сообщает, что остров открыт или закрыт (например, открыли кликом)
#[tauri::command]
fn set_open(v: bool) { OPEN.store(v, Ordering::Relaxed); }

#[tauri::command]
fn quit(app: tauri::AppHandle) { app.exit(0); }

fn set_through(win: &tauri::WebviewWindow, cur: &mut bool, want: bool) {
    if *cur != want { let _ = win.set_ignore_cursor_events(want); *cur = want; }
}

#[cfg(windows)]
fn button_down() -> bool { sys::input::button_down() }
#[cfg(not(windows))]
fn button_down() -> bool { false }
#[cfg(windows)]
fn fullscreen_app() -> bool { sys::input::fullscreen_app() }
#[cfg(not(windows))]
fn fullscreen_app() -> bool { false }

// Следит за мышью. Остров открывается, когда мышь постояла у самого верха экрана
// по центру (не двигаясь, без зажатой кнопки и не поверх полноэкранного приложения),
// или по горячей клавише. Закрывается, когда мышь уходит с острова.
// Пока остров открыт, окно пропускает клики везде, кроме самого острова.
fn watch_cursor(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let (mut outside, mut since_open, mut dwell) = (0u32, 0u32, 0u32);
        let (mut pinned, mut touched) = (false, false);
        let (mut last_size, mut grace) = ((0u64, 0u64), 0u32);
        let mut last: Option<(f64, f64)> = None;
        let mut through = true;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(40));
            let Some(win) = app.get_webview_window("main") else { continue };
            let (Ok(c), Ok(Some(m))) = (win.cursor_position(), win.primary_monitor()) else { continue };
            let sf = m.scale_factor();
            let (mx, my) = (m.position().x as f64, m.position().y as f64);
            let mw = m.size().width as f64;
            let moved = last.map(|(lx, ly)| ((c.x - lx).powi(2) + (c.y - ly).powi(2)).sqrt()).unwrap_or(0.0);
            last = Some((c.x, c.y));

            if FORCE.swap(false, Ordering::Relaxed) {
                dwell = 0;
                if OPEN.load(Ordering::Relaxed) {
                    OPEN.store(false, Ordering::Relaxed);
                    pinned = false;
                    let _ = app.emit("island-close", ());
                } else {
                    OPEN.store(true, Ordering::Relaxed);
                    (outside, since_open) = (0, 0);
                    (pinned, touched) = (true, false);
                    let _ = app.emit("island-open", ());
                }
            }

            if !OPEN.load(Ordering::Relaxed) {
                set_through(&win, &mut through, true);
                // зона открытия: узкая, около 120 пикселей по центру
                let hot = !PAUSED.load(Ordering::Relaxed)
                    && c.y >= my && c.y - my <= 3.0
                    && (c.x - (mx + mw / 2.0)).abs() <= 60.0 * sf;
                if hot && moved <= 10.0 * sf && !button_down() { dwell += 1 } else { dwell = 0 }
                // около 0,3 с покоя у края
                if dwell >= 8 {
                    dwell = 0;
                    if !fullscreen_app() {
                        OPEN.store(true, Ordering::Relaxed);
                        (outside, since_open) = (0, 0);
                        (pinned, touched) = (false, false);
                        let _ = app.emit("island-open", ());
                    }
                }
            } else {
                since_open += 1;
                let size_now = (ISL_W.load(Ordering::Relaxed), ISL_H.load(Ordering::Relaxed));
                if size_now != last_size {
                    // остров сменил размер (другая вкладка): даём мыши время, не закрываемся
                    last_size = size_now;
                    grace = since_open + 40;
                    outside = 0;
                }
                let (Ok(p), Ok(s)) = (win.outer_position(), win.outer_size()) else { continue };
                let (iw, ih) = (f64::from_bits(ISL_W.load(Ordering::Relaxed)) * sf, f64::from_bits(ISL_H.load(Ordering::Relaxed)) * sf);
                let cx = p.x as f64 + s.width as f64 / 2.0;
                let top = p.y as f64 + 12.0 * sf;
                let inside_in = |pad: f64| c.x >= cx - iw / 2.0 - pad && c.x <= cx + iw / 2.0 + pad
                    && c.y >= top - 14.0 * sf && c.y <= top + ih + pad;
                let inside = iw > 0.0 && inside_in(10.0 * sf);
                // клики проходят сквозь окно везде, кроме острова
                set_through(&win, &mut through, !(iw > 0.0 && inside_in(4.0 * sf)));
                if inside { outside = 0; touched = true } else { outside += 1 }
                let close = if pinned && !touched {
                    since_open > 150 // открыли клавишей, мышь не трогали: закроется через 6 секунд
                } else {
                    outside >= 20 && since_open > 15 && since_open > grace // около 0,8 с вне острова
                };
                if close {
                    OPEN.store(false, Ordering::Relaxed);
                    pinned = false;
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
            get_volume, set_volume, get_brightness, set_brightness, net_names, open_settings,
            shape, set_open, quit
        ])
        .setup(|app| {
            // Положение окон: по центру вверху основного экрана
            let (mut wx, mut wy) = (0.0, 0.0);
            if let Some(win) = app.get_webview_window("main") {
                if let Ok(Some(m)) = win.primary_monitor() {
                    let sf = m.scale_factor();
                    wx = m.position().x as f64 / sf + (m.size().width as f64 / sf - WIN_W) / 2.0;
                    wy = m.position().y as f64 / sf;
                }
                let _ = win.set_size(tauri::LogicalSize::new(WIN_W, WIN_H));
                let _ = win.set_position(tauri::LogicalPosition::new(wx, wy));
                let _ = win.set_ignore_cursor_events(true);
            }

            // Подложка с размытием: создаётся и показывается раньше основного окна, поэтому лежит под ним
            if let Ok(b) = tauri::WebviewWindowBuilder::new(app, "blur", tauri::WebviewUrl::App("blur.html".into()))
                .title("Glow blur").decorations(false).transparent(true).always_on_top(true)
                .skip_taskbar(true).resizable(false).shadow(false).focused(false).focusable(false)
                .visible(false).inner_size(WIN_W, WIN_H).position(wx, wy).build()
            {
                let _ = b.set_ignore_cursor_events(true);
                #[cfg(windows)]
                {
                    if let Ok(hw) = b.hwnd() { sys::blur::set_region(hw.0 as isize, 0, 0, 0, 0, 0, false); }
                    if window_vibrancy::apply_blur(&b, Some((0, 0, 0, 0))).is_err() {
                        let _ = window_vibrancy::apply_acrylic(&b, Some((0, 0, 0, 0)));
                    }
                }
                let _ = b.show();
            }
            if let Some(win) = app.get_webview_window("main") { let _ = win.show(); }

            watch_cursor(app.handle().clone());

            // Горячая клавиша Ctrl+Alt+G: открыть или закрыть остров
            #[cfg(windows)]
            std::thread::spawn(|| sys::input::hotkey_loop(|| FORCE.store(true, Ordering::Relaxed)));

            // Значок в трее: пауза, открыть, выход
            let open_i = MenuItem::with_id(app, "open", "Открыть Glow (Ctrl+Alt+G)", true, None::<&str>)?;
            let pause_i = CheckMenuItem::with_id(app, "pause", "Не открывать при наведении", true, false, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open_i, &pause_i, &quit_i])?;
            let pause_c = pause_i.clone();
            let mut tray = TrayIconBuilder::new().tooltip("Glow").menu(&menu)
                .on_menu_event(move |app, ev| match ev.id.as_ref() {
                    "open" => FORCE.store(true, Ordering::Relaxed),
                    "pause" => PAUSED.store(pause_c.is_checked().unwrap_or(false), Ordering::Relaxed),
                    "quit" => app.exit(0),
                    _ => {}
                });
            if let Some(icon) = app.default_window_icon() { tray = tray.icon(icon.clone()); }
            let _ = tray.build(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("не удалось запустить Glow");
}
