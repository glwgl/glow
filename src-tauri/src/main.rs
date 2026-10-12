#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
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
// Размытие фона под островом: включено ли и последний снимок экрана (BMP в base64)
static BLUR_ON: AtomicBool = AtomicBool::new(true);
static BG: Mutex<Option<String>> = Mutex::new(None);

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
const WIN_H: f64 = 620.0;

// Интерфейс сообщает размер, до которого остров вырастет (для слежения за мышью).
// Нули: остров закрыт.
#[tauri::command]
fn shape(tw: f64, th: f64) {
    ISL_W.store(tw.to_bits(), Ordering::Relaxed);
    ISL_H.store(th.to_bits(), Ordering::Relaxed);
}

// Включить или выключить размытие фона под островом
#[tauri::command]
fn set_blur(on: bool) { BLUR_ON.store(on, Ordering::Relaxed); }

// Забрать снимок фона, сделанный перед открытием острова
#[tauri::command]
fn get_bg() -> Option<String> { BG.lock().ok().and_then(|g| g.clone()) }

// Автозапуск вместе с Windows
#[tauri::command]
fn get_autostart() -> bool {
    #[cfg(windows)]
    { sys::autostart::get() }
    #[cfg(not(windows))]
    { false }
}

#[tauri::command]
fn set_autostart(on: bool) -> bool {
    #[cfg(windows)]
    { sys::autostart::set(on) }
    #[cfg(not(windows))]
    { let _ = on; false }
}

// Снимок экрана под окном приложения: вызывается прямо перед открытием острова
fn snap_bg(app: &tauri::AppHandle) {
    if !BLUR_ON.load(Ordering::Relaxed) { return; }
    #[cfg(windows)]
    if let Some(win) = app.get_webview_window("main") {
        if let (Ok(p), Ok(s), Ok(h)) = (win.outer_position(), win.outer_size(), win.hwnd()) {
            let img = sys::snap::grab(h.0 as isize, p.x, p.y, s.width as i32, s.height as i32, 3);
            if let Ok(mut g) = BG.lock() { *g = img; }
        }
    }
    #[cfg(not(windows))]
    { let _ = app; }
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
        // тик цикла: 40 мс
        let (mut outside, mut since_open, mut dwell) = (0u32, 0u32, 0u32);
        let (mut pinned, mut touched) = (false, false);
        let (mut last_size, mut grace, mut hold_h) = ((0u64, 0u64), 0u32, 0.0f64);
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
                    snap_bg(&app);
                    OPEN.store(true, Ordering::Relaxed);
                    (outside, since_open, grace, hold_h, last_size) = (0, 0, 0, 0.0, (0, 0));
                    (pinned, touched) = (true, false);
                    let _ = app.emit("island-open", ());
                }
            }

            if !OPEN.load(Ordering::Relaxed) {
                set_through(&win, &mut through, true);
                // зона открытия: узкая, около 120 пикселей по центру, у самого верхнего края
                let hot = !PAUSED.load(Ordering::Relaxed)
                    && c.y >= my && c.y - my <= 3.0
                    && (c.x - (mx + mw / 2.0)).abs() <= 60.0 * sf;
                // открываемся только когда мышь остановилась: любое заметное движение начинает отсчёт заново
                let still = moved <= 1.5 * sf;
                if hot && still && !button_down() { dwell += 1 } else { dwell = 0 }
                // около 0,3 с полного покоя у края
                if dwell >= 8 {
                    dwell = 0;
                    if !fullscreen_app() {
                        snap_bg(&app);
                        OPEN.store(true, Ordering::Relaxed);
                        (outside, since_open, grace, hold_h, last_size) = (0, 0, 0, 0.0, (0, 0));
                        (pinned, touched) = (false, false);
                        let _ = app.emit("island-open", ());
                    }
                }
            } else {
                since_open += 1;
                let size_now = (ISL_W.load(Ordering::Relaxed), ISL_H.load(Ordering::Relaxed));
                let (iw, ih) = (f64::from_bits(size_now.0) * sf, f64::from_bits(size_now.1) * sf);
                if size_now != last_size {
                    // остров сменил размер из-за другой вкладки (а не при самом открытии): 2 секунды
                    // не закрываем и считаем островом ещё и прежнюю высоту, чтобы мышь на месте
                    // кнопки вкладки не оказалась «снаружи»
                    if f64::from_bits(last_size.1) > 0.0 {
                        hold_h = f64::from_bits(last_size.1) * sf;
                        grace = since_open + 50;
                        outside = 0;
                    }
                    last_size = size_now;
                }
                let (Ok(p), Ok(s)) = (win.outer_position(), win.outer_size()) else { continue };
                let cx = p.x as f64 + s.width as f64 / 2.0;
                let top = p.y as f64 + 12.0 * sf;
                let ih_eff = if since_open <= grace { ih.max(hold_h) } else { ih };
                // расстояние от мыши до острова в логических пикселях (0, если мышь над ним)
                let dx = ((cx - iw / 2.0) - c.x).max(c.x - (cx + iw / 2.0)).max(0.0);
                let dy = ((top - 14.0 * sf) - c.y).max(c.y - (top + ih_eff)).max(0.0);
                let dist = (dx * dx + dy * dy).sqrt() / sf;
                let inside = iw > 0.0 && dist <= 10.0;
                // клики проходят сквозь окно везде, кроме острова
                set_through(&win, &mut through, !(iw > 0.0 && dist <= 4.0));
                if inside { outside = 0; touched = true } else { outside += 1 }
                // чем дальше мышь, тем быстрее закрытие: далеко 0,08 с, средне 0,2 с, рядом 0,36 с
                let need = if dist > 120.0 { 2 } else if dist > 50.0 { 5 } else { 9 };
                let close = if pinned && !touched {
                    since_open > 150 // открыли клавишей, мышь не трогали: закроется через 6 секунд
                } else {
                    outside >= need && since_open > 6 && since_open > grace
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
            shape, set_blur, get_bg, get_autostart, set_autostart, set_open, quit
        ])
        .setup(|app| {
            // Положение окна: по центру вверху основного экрана
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

            if let Some(win) = app.get_webview_window("main") { let _ = win.show(); }

            // Автозапуск включён: обновляем путь к программе в реестре (вдруг её переустановили)
            #[cfg(windows)]
            if sys::autostart::get() { let _ = sys::autostart::set(true); }

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
