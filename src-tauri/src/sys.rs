// Всё, что общается с Windows: медиа, радиомодули (Wi-Fi / Bluetooth), звук, яркость.
use base64::Engine;
use serde::Serialize;
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::Mutex;
use windows::core::Result;

const NO_WINDOW: u32 = 0x0800_0000;

#[derive(Serialize)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub position: f64,
    pub duration: f64,
    pub playing: bool,
}

#[derive(Serialize)]
pub struct Radios {
    pub wifi: Option<bool>,
    pub bt: Option<bool>,
    pub air: Option<bool>,
}

// ---------- Музыка ----------
pub mod media {
    use super::*;
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSession as Sess,
        GlobalSystemMediaTransportControlsSessionManager as Mgr,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as PS,
    };
    use windows::Storage::Streams::DataReader;

    // Обложку читаем один раз на трек и кэшируем
    static COVER: Mutex<(String, Option<String>)> = Mutex::new((String::new(), None));

    pub fn session() -> Result<Sess> {
        Mgr::RequestAsync()?.get()?.GetCurrentSession()
    }

    pub fn read() -> Result<Track> {
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

    pub fn cmd(c: &str) -> Result<()> {
        let s = session()?;
        match c {
            "toggle" => { s.TryTogglePlayPauseAsync()?.get()?; }
            "next" => { s.TrySkipNextAsync()?.get()?; }
            _ => { s.TrySkipPreviousAsync()?.get()?; }
        }
        Ok(())
    }

    // Перемотка: позиция в тиках по 100 нс
    pub fn seek(secs: f64) -> Result<()> {
        let s = session()?;
        let ticks = (secs.max(0.0) * 1e7) as i64;
        s.TryChangePlaybackPositionAsync(ticks)?.get()?;
        Ok(())
    }

    pub fn cover(key: &str) -> Option<String> {
        if let Ok(c) = COVER.lock() {
            if c.0 == key { if let Some(v) = &c.1 { return Some(v.clone()); } }
        }
        let v = read_cover().ok()?;
        if let Ok(mut c) = COVER.lock() { *c = (key.to_string(), Some(v.clone())); }
        Some(v)
    }

    fn read_cover() -> Result<String> {
        let s = session()?;
        let p = s.TryGetMediaPropertiesAsync()?.get()?;
        let st = p.Thumbnail()?.OpenReadAsync()?.get()?;
        let size = st.Size()? as u32;
        if size == 0 || size > 8_000_000 {
            return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2147467259)));
        }
        let input = st.GetInputStreamAt(0)?;
        let rd = DataReader::CreateDataReader(&input)?;
        rd.LoadAsync(size)?.get()?;
        let mut buf = vec![0u8; size as usize];
        rd.ReadBytes(&mut buf)?;
        let mime = st.ContentType()?.to_string();
        let mime = if mime.is_empty() { "image/jpeg".to_string() } else { mime };
        Ok(format!("data:{};base64,{}", mime, base64::engine::general_purpose::STANDARD.encode(&buf)))
    }
}

// ---------- Wi-Fi, Bluetooth, режим «в самолёте» ----------
pub mod radio {
    use super::*;
    use windows::Devices::Radios::{Radio, RadioKind, RadioState};

    pub fn get() -> Result<Radios> {
        let _ = Radio::RequestAccessAsync()?.get()?;
        let list = Radio::GetRadiosAsync()?.get()?;
        let (mut wifi, mut bt): (Option<bool>, Option<bool>) = (None, None);
        let (mut any, mut any_on) = (false, false);
        for r in list {
            let on = r.State()? == RadioState::On;
            match r.Kind()? {
                RadioKind::WiFi => wifi = Some(wifi.unwrap_or(false) || on),
                RadioKind::Bluetooth => bt = Some(bt.unwrap_or(false) || on),
                _ => {}
            }
            any = true;
            any_on |= on;
        }
        Ok(Radios { wifi, bt, air: if any { Some(!any_on) } else { None } })
    }

    // kind: "wifi" | "bt" | "air". Для "air" on=true значит «включить режим в самолёте»
    pub fn set(kind: &str, on: bool) -> Result<()> {
        let _ = Radio::RequestAccessAsync()?.get()?;
        let list = Radio::GetRadiosAsync()?.get()?;
        let state = if (kind == "air") != on { RadioState::On } else { RadioState::Off };
        for r in list {
            let k = r.Kind()?;
            let hit = match kind {
                "wifi" => k == RadioKind::WiFi,
                "bt" => k == RadioKind::Bluetooth,
                "air" => true,
                _ => false,
            };
            if hit { let _ = r.SetStateAsync(state)?.get()?; }
        }
        Ok(())
    }
}

// ---------- Громкость ----------
pub mod volume {
    use windows::core::Result;
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};

    fn endpoint() -> Result<IAudioEndpointVolume> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
            dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
        }
    }
    pub fn get() -> Result<f32> {
        unsafe { endpoint()?.GetMasterVolumeLevelScalar() }
    }
    pub fn set(v: f32) -> Result<()> {
        unsafe { endpoint()?.SetMasterVolumeLevelScalar(v.clamp(0.0, 1.0), std::ptr::null()) }
    }
}

// ---------- Яркость (экраны ноутбуков, через WMI) ----------
pub mod brightness {
    use super::*;
    fn ps(script: &str) -> Option<String> {
        let o = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", script])
            .creation_flags(NO_WINDOW)
            .output().ok()?;
        if !o.status.success() { return None; }
        Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
    }
    pub fn get() -> Option<u32> {
        ps("(Get-CimInstance -Namespace root/WMI -ClassName WmiMonitorBrightness | Select-Object -First 1).CurrentBrightness")?
            .parse().ok()
    }
    pub fn set(v: u32) -> bool {
        let v = v.min(100);
        ps(&format!("Get-CimInstance -Namespace root/WMI -ClassName WmiMonitorBrightnessMethods | Invoke-CimMethod -MethodName WmiSetBrightness -Arguments @{{Timeout=1;Brightness={}}} | Out-Null", v)).is_some()
    }
}

// ---------- Параметры Windows ----------
pub fn open_settings(uri: &str) {
    if !uri.starts_with("ms-settings:") { return; }
    let _ = Command::new("cmd").args(["/C", "start", "", uri]).creation_flags(NO_WINDOW).spawn();
}
