#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use serde::Serialize;
use tauri::Manager;

#[derive(Serialize)]
struct Track { title: String, artist: String, position: f64, duration: f64, playing: bool }

#[cfg(windows)]
mod media {
    use super::Track;
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSession as Sess,
        GlobalSystemMediaTransportControlsSessionManager as Mgr,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as PS,
    };
    pub fn session() -> windows::core::Result<Sess> {
        Mgr::RequestAsync()?.get()?.GetCurrentSession()
    }
    pub fn read() -> windows::core::Result<Track> {
        let s = session()?;
        let p = s.TryGetMediaPropertiesAsync()?.get()?;
        let t = s.GetTimelineProperties()?;
        Ok(Track {
            title: p.Title()?.to_string(),
            artist: p.Artist()?.to_string(),
            position: t.Position()?.Duration as f64 / 1e7,
            duration: t.EndTime()?.Duration as f64 / 1e7,
            playing: s.GetPlaybackInfo()?.PlaybackStatus()? == PS::Playing,
        })
    }
    pub fn cmd(c: &str) -> windows::core::Result<()> {
        let s = session()?;
        match c {
            "toggle" => { s.TryTogglePlayPauseAsync()?.get()?; }
            "next" => { s.TrySkipNextAsync()?.get()?; }
            _ => { s.TrySkipPreviousAsync()?.get()?; }
        }
        Ok(())
    }
}

// Что играет сейчас в любом плеере (системные медиасессии Windows)
#[tauri::command]
async fn now_playing() -> Option<Track> {
    #[cfg(windows)] { media::read().ok() }
    #[cfg(not(windows))] { None }
}

#[tauri::command]
async fn media_cmd(cmd: String) {
    #[cfg(windows)] { let _ = media::cmd(&cmd); }
    #[cfg(not(windows))] { let _ = cmd; }
}

#[tauri::command]
fn battery_info() -> Option<(f32, bool)> {
    let m = battery::Manager::new().ok()?;
    let b = m.batteries().ok()?.next()?.ok()?;
    Some((b.state_of_charge().value * 100.0, b.state() == battery::State::Charging))
}

// Подгоняет окно под размер острова и центрирует его вверху экрана
#[tauri::command]
fn fit(win: tauri::WebviewWindow, w: f64, h: f64) {
    let _ = win.set_size(tauri::LogicalSize::new(w, h));
    if let Ok(Some(m)) = win.primary_monitor() {
        let x = (m.size().width as f64 / m.scale_factor() - w) / 2.0;
        let _ = win.set_position(tauri::LogicalPosition::new(x, 0.0));
    }
}

#[tauri::command]
fn quit(app: tauri::AppHandle) { app.exit(0); }

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![now_playing, media_cmd, battery_info, fit, quit])
        .setup(|app| { let _ = app.get_webview_window("main"); Ok(()) })
        .run(tauri::generate_context!())
        .expect("не удалось запустить Glow");
}
