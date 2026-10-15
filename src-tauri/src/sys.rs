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
        let info = s.GetPlaybackInfo()?;
        let playing = info.PlaybackStatus()? == PS::Playing;
        let duration = t.EndTime()?.Duration as f64 / 1e7;
        let mut position = t.Position()?.Duration as f64 / 1e7;
        // Плеер сообщает позицию редко. Достраиваем её до текущего момента:
        // позиция + время, прошедшее с последнего обновления (иначе текст «дёргается»)
        if playing {
            if let Ok(last) = t.LastUpdatedTime() {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| (d.as_nanos() / 100) as i64)
                    .unwrap_or(0) + 116_444_736_000_000_000i64;
                let elapsed = (now - last.UniversalTime) as f64 / 1e7;
                if elapsed > 0.0 && elapsed < 43_200.0 {
                    let rate = info.PlaybackRate().ok().and_then(|r| r.Value().ok()).unwrap_or(1.0);
                    position += elapsed * rate;
                }
            }
            if duration > 0.0 && position > duration { position = duration; }
        }
        Ok(Track {
            title: p.Title()?.to_string(),
            artist: p.Artist()?.to_string(),
            position,
            duration,
            playing,
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
// Для установки держим один PowerShell открытым и шлём ему команды: так ползунок
// двигает яркость сразу, без запуска нового процесса на каждое движение.
pub mod brightness {
    use super::*;
    use std::io::Write;
    use std::process::{Child, Stdio};
    static PS: Mutex<Option<Child>> = Mutex::new(None);

    pub fn get() -> Option<u32> {
        let o = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command",
                   "(Get-CimInstance -Namespace root/WMI -ClassName WmiMonitorBrightness | Select-Object -First 1).CurrentBrightness"])
            .creation_flags(NO_WINDOW)
            .output().ok()?;
        if !o.status.success() { return None; }
        String::from_utf8_lossy(&o.stdout).trim().parse().ok()
    }

    pub fn set(v: u32) -> bool {
        let Ok(mut g) = PS.lock() else { return false };
        let dead = match g.as_mut() {
            Some(c) => c.try_wait().map(|s| s.is_some()).unwrap_or(true),
            None => true,
        };
        if dead {
            let child = Command::new("powershell")
                .args(["-NoProfile", "-NoLogo", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", "-"])
                .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null())
                .creation_flags(NO_WINDOW)
                .spawn();
            match child {
                Ok(mut c) => {
                    if let Some(i) = c.stdin.as_mut() {
                        let _ = writeln!(i, "$m = Get-CimInstance -Namespace root/WMI -ClassName WmiMonitorBrightnessMethods");
                    }
                    *g = Some(c);
                }
                Err(_) => return false,
            }
        }
        if let Some(i) = g.as_mut().and_then(|c| c.stdin.as_mut()) {
            return writeln!(i, "Invoke-CimMethod -InputObject $m -MethodName WmiSetBrightness -Arguments @{{Timeout=1;Brightness={}}} | Out-Null", v.min(100))
                .and_then(|_| i.flush()).is_ok();
        }
        false
    }
}

// ---------- Блокировка экрана и сон ----------
pub mod session {
    use std::time::{Duration, SystemTime};

    #[link(name = "user32")]
    extern "system" {
        fn OpenInputDesktop(flags: u32, inherit: i32, access: u32) -> isize;
        fn CloseDesktop(desk: isize) -> i32;
    }

    // Пока экран заблокирован, рабочий стол ввода недоступен, и Windows возвращает 0
    pub fn locked() -> bool {
        unsafe {
            let h = OpenInputDesktop(0, 0, 0x0100); // DESKTOP_SWITCHDESKTOP
            if h == 0 { true } else { CloseDesktop(h); false }
        }
    }

    // Вызывает on_unlock, когда экран разблокировали
    // или когда компьютер проснулся из сна без экрана блокировки
    pub fn watch(on_unlock: impl Fn() + Send + 'static) {
        let mut was_locked = locked();
        let mut last = SystemTime::now();
        loop {
            std::thread::sleep(Duration::from_millis(1000));
            let gap = SystemTime::now().duration_since(last).unwrap_or_default();
            last = SystemTime::now();
            let now = locked();
            if !now && (was_locked || gap.as_secs() >= 8) { on_unlock(); }
            was_locked = now;
        }
    }
}

// ---------- Система: загрузка процессора, память, диски, температура ----------
// Вызовы kernel32 объявлены напрямую: так не нужны дополнительные возможности крейта windows.
pub mod stats {
    use super::*;
    use std::time::Instant;

    #[repr(C)]
    struct MemStatus {
        len: u32,
        load: u32,
        total: u64,
        avail: u64,
        total_page: u64,
        avail_page: u64,
        total_virt: u64,
        avail_virt: u64,
        avail_ext: u64,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetSystemTimes(idle: *mut u64, kernel: *mut u64, user: *mut u64) -> i32;
        fn GlobalMemoryStatusEx(buf: *mut MemStatus) -> i32;
        fn GetLogicalDrives() -> u32;
        fn GetDriveTypeW(root: *const u16) -> u32;
        fn GetDiskFreeSpaceExW(dir: *const u16, avail: *mut u64, total: *mut u64, free: *mut u64) -> i32;
        fn SetErrorMode(mode: u32) -> u32;
    }

    const GB: f64 = 1_073_741_824.0;

    // Процессор: (занято, всего) на прошлый замер
    static PREV: Mutex<Option<(u64, u64)>> = Mutex::new(None);

    fn times() -> Option<(u64, u64)> {
        let (mut idle, mut kernel, mut user) = (0u64, 0u64, 0u64);
        if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 { return None; }
        // время ядра уже включает простой
        Some(((kernel + user).saturating_sub(idle), kernel + user))
    }

    fn pct(a: (u64, u64), b: (u64, u64)) -> Option<f32> {
        let busy = b.0.saturating_sub(a.0);
        let total = b.1.saturating_sub(a.1);
        if total == 0 { return None; }
        Some((busy as f64 / total as f64 * 100.0).clamp(0.0, 100.0) as f32)
    }

    pub fn cpu() -> Option<f32> {
        let now = times()?;
        let prev = {
            let mut g = PREV.lock().ok()?;
            let p = *g;
            *g = Some(now);
            p
        };
        match prev {
            Some(p) => pct(p, now),
            None => {
                // самый первый замер: ждём чуть-чуть, чтобы было с чем сравнивать
                std::thread::sleep(std::time::Duration::from_millis(250));
                let n2 = times()?;
                if let Ok(mut g) = PREV.lock() { *g = Some(n2); }
                pct(now, n2)
            }
        }
    }

    // Память: (процент, занято ГБ, всего ГБ)
    pub fn mem() -> Option<(u32, f64, f64)> {
        let mut m = MemStatus {
            len: std::mem::size_of::<MemStatus>() as u32,
            load: 0, total: 0, avail: 0, total_page: 0, avail_page: 0, total_virt: 0, avail_virt: 0, avail_ext: 0,
        };
        if unsafe { GlobalMemoryStatusEx(&mut m) } == 0 || m.total == 0 { return None; }
        Some((m.load, m.total.saturating_sub(m.avail) as f64 / GB, m.total as f64 / GB))
    }

    // Диски: (имя, занято ГБ, всего ГБ). Обычные и съёмные (флешки), пустые картридеры пропускаем.
    pub fn disks() -> Vec<(String, f64, f64)> {
        unsafe { SetErrorMode(1); } // без окон «нет диска в устройстве»
        let mask = unsafe { GetLogicalDrives() };
        let mut out = Vec::new();
        for i in 2..26u32 { // с C: (A: и B: дискеты)
            if mask & (1u32 << i) == 0 { continue; }
            let name = format!("{}:\\", (b'A' + i as u8) as char);
            let root: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
            if kind != 3 && kind != 2 { continue; } // 3 жёсткий диск, 2 съёмный
            let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
            if unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut avail, &mut total, &mut free) } == 0 || total == 0 { continue; }
            out.push((name, total.saturating_sub(free) as f64 / GB, total as f64 / GB));
        }
        out
    }

    // Температура: датчик тепловой зоны ACPI. Есть не на каждом компьютере, тогда вернётся None.
    static TEMP: Mutex<(Option<Instant>, Option<f32>)> = Mutex::new((None, None));

    fn read_temp() -> Option<f32> {
        let script = "$t=(Get-CimInstance Win32_PerfFormattedData_Counters_ThermalZoneInformation -ErrorAction SilentlyContinue | Measure-Object HighPrecisionTemperature -Maximum).Maximum; \
                      if($t){$t}else{(Get-CimInstance -Namespace root/wmi MSAcpi_ThermalZoneTemperature -ErrorAction SilentlyContinue | Measure-Object CurrentTemperature -Maximum).Maximum}";
        let o = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", script])
            .creation_flags(NO_WINDOW)
            .output().ok()?;
        if !o.status.success() { return None; }
        let tenth_kelvin: f64 = String::from_utf8_lossy(&o.stdout).trim().replace(',', ".").parse().ok()?;
        let c = tenth_kelvin / 10.0 - 273.15;
        if (1.0..130.0).contains(&c) { Some(c as f32) } else { None }
    }

    pub fn temp() -> Option<f32> {
        let (at, val) = *TEMP.lock().ok()?;
        if let Some(t) = at {
            // есть значение: не чаще раза в 4 секунды; датчика нет: пробуем снова раз в минуту
            let ttl = if val.is_some() { 4 } else { 60 };
            if t.elapsed().as_secs() < ttl { return val; }
        }
        let v = read_temp();
        if let Ok(mut g) = TEMP.lock() { *g = (Some(Instant::now()), v); }
        v
    }
}

// ---------- Имена: к какому Wi-Fi и каким Bluetooth-устройствам подключён компьютер ----------
pub mod names {
    use windows::Devices::Bluetooth::{BluetoothConnectionStatus, BluetoothDevice, BluetoothLEDevice};
    use windows::Devices::Enumeration::DeviceInformation;
    use windows::Networking::Connectivity::{NetworkConnectivityLevel, NetworkInformation};

    pub fn wifi() -> Option<String> {
        let list = NetworkInformation::GetConnectionProfiles().ok()?;
        for p in list {
            if !p.IsWlanConnectionProfile().unwrap_or(false) { continue; }
            let lvl = p.GetNetworkConnectivityLevel().unwrap_or(NetworkConnectivityLevel::None);
            if lvl == NetworkConnectivityLevel::None { continue; }
            if let Ok(n) = p.ProfileName() {
                let n = n.to_string();
                if !n.is_empty() { return Some(n); }
            }
        }
        None
    }

    pub fn bluetooth() -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let sels = [
            BluetoothDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected),
            BluetoothLEDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected),
        ];
        for sel in sels {
            let Ok(sel) = sel else { continue };
            let Ok(op) = DeviceInformation::FindAllAsyncAqsFilter(&sel) else { continue };
            let Ok(found) = op.get() else { continue };
            for d in found {
                if let Ok(n) = d.Name() {
                    let n = n.to_string();
                    if !n.is_empty() && !out.contains(&n) { out.push(n); }
                }
            }
        }
        out
    }
}

// ---------- Снимок фона под островом ----------
// Размытие делаем сами: снимаем кусок экрана под окном приложения (уменьшенный), а интерфейс
// размывает его и кладёт под остров. Системные эффекты размытия окон не используются,
// поэтому чёрных прямоугольников быть не может: если снимок не получился, остров просто без размытия.
pub mod snap {
    use base64::Engine;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WINDOW_DISPLAY_AFFINITY};

    // 0x11: окно не попадает в снимки экрана (Windows 10 2004 и новее), 0: обычный режим
    const EXCLUDE: WINDOW_DISPLAY_AFFINITY = WINDOW_DISPLAY_AFFINITY(0x11);
    const NORMAL: WINDOW_DISPLAY_AFFINITY = WINDOW_DISPLAY_AFFINITY(0);

    // x, y, w, h: прямоугольник экрана в физических пикселях; div: во сколько раз уменьшить
    pub fn grab(hwnd: isize, x: i32, y: i32, w: i32, h: i32, div: i32) -> Option<String> {
        if w <= 0 || h <= 0 { return None; }
        unsafe {
            let hw = HWND(hwnd as *mut core::ffi::c_void);
            // на время снимка прячем само окно Glow, чтобы остров не снялся сам на себя
            let hidden = SetWindowDisplayAffinity(hw, EXCLUDE).is_ok();
            if hidden { std::thread::sleep(std::time::Duration::from_millis(60)); }
            let res = capture(x, y, w, h, div.max(1));
            if hidden { let _ = SetWindowDisplayAffinity(hw, NORMAL); }
            res
        }
    }

    unsafe fn capture(x: i32, y: i32, w: i32, h: i32, div: i32) -> Option<String> {
        let (dw, dh) = ((w / div).max(1), (h / div).max(1));
        let screen = GetDC(HWND(core::ptr::null_mut()));
        if screen.is_invalid() { return None; }
        let mem = CreateCompatibleDC(screen);
        let bmp = CreateCompatibleBitmap(screen, dw, dh);
        let old = SelectObject(mem, HGDIOBJ(bmp.0));
        let _ = SetStretchBltMode(mem, HALFTONE);
        let _ = SetBrushOrgEx(mem, 0, 0, None);
        let copied = StretchBlt(mem, 0, 0, dw, dh, screen, x, y, w, h,
                                ROP_CODE(SRCCOPY.0 | CAPTUREBLT.0)).as_bool();
        // GetDIBits работает только с картинкой, которая ни в какой контекст не выбрана
        SelectObject(mem, old);
        let mut pix = vec![0u8; (dw as usize) * (dh as usize) * 4];
        let mut bi: BITMAPINFO = core::mem::zeroed();
        bi.bmiHeader.biSize = core::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = dw;
        bi.bmiHeader.biHeight = dh; // положительная высота: строки идут снизу вверх, как ждёт формат BMP
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB.0;
        let lines = GetDIBits(mem, bmp, 0, dh as u32, Some(pix.as_mut_ptr() as *mut core::ffi::c_void),
                              &mut bi, DIB_RGB_COLORS);
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(mem);
        ReleaseDC(HWND(core::ptr::null_mut()), screen);
        if !copied || lines == 0 { return None; }
        for px in pix.chunks_exact_mut(4) { px[3] = 255; }
        let mut out: Vec<u8> = Vec::with_capacity(54 + pix.len());
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&((54 + pix.len()) as u32).to_le_bytes());
        out.extend_from_slice(&[0u8; 4]);
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&dw.to_le_bytes());
        out.extend_from_slice(&dh.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(pix.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0u8; 16]);
        out.extend_from_slice(&pix);
        Some(base64::engine::general_purpose::STANDARD.encode(out))
    }
}

// ---------- Автозапуск вместе с Windows ----------
// Запись в разделе Run текущего пользователя: права администратора не нужны.
pub mod autostart {
    use windows::core::w;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
    };

    pub fn get() -> bool {
        unsafe {
            RegGetValueW(HKEY_CURRENT_USER, w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
                         w!("Glow"), RRF_RT_REG_SZ, None, None, None) == ERROR_SUCCESS
        }
    }

    // Возвращает true, если после вызова состояние такое, как просили
    pub fn set(on: bool) -> bool {
        unsafe {
            if on {
                let Ok(exe) = std::env::current_exe() else { return false };
                let val: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain(Some(0)).collect();
                RegSetKeyValueW(HKEY_CURRENT_USER, w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
                                w!("Glow"), REG_SZ.0, Some(val.as_ptr() as *const core::ffi::c_void),
                                (val.len() * 2) as u32) == ERROR_SUCCESS
            } else {
                let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"), w!("Glow"));
                !get()
            }
        }
    }
}

// ---------- Мышь, клавиши, полноэкранные приложения ----------
pub mod input {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, RegisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, VK_LBUTTON, VK_RBUTTON,
    };
    use windows::Win32::UI::Shell::{
        SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};

    // Зажата ли левая или правая кнопка мыши
    pub fn button_down() -> bool {
        unsafe {
            (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0
                || (GetAsyncKeyState(VK_RBUTTON.0 as i32) as u16 & 0x8000) != 0
        }
    }

    // Идёт ли полноэкранное приложение (игра, видео на весь экран, презентация)
    pub fn fullscreen_app() -> bool {
        unsafe {
            match SHQueryUserNotificationState() {
                Ok(s) => s == QUNS_BUSY || s == QUNS_RUNNING_D3D_FULL_SCREEN || s == QUNS_PRESENTATION_MODE,
                Err(_) => false,
            }
        }
    }

    // Горячая клавиша Ctrl+Alt+G. Цикл сообщений живёт в своём потоке.
    pub fn hotkey_loop(on_press: impl Fn()) {
        unsafe {
            if RegisterHotKey(HWND(core::ptr::null_mut()), 1, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 0x47).is_err() {
                return;
            }
            let mut msg: MSG = core::mem::zeroed();
            while GetMessageW(&mut msg, HWND(core::ptr::null_mut()), 0, 0).as_bool() {
                if msg.message == WM_HOTKEY { on_press(); }
            }
        }
    }
}

// ---------- Параметры Windows ----------
pub fn open_settings(uri: &str) {
    if !uri.starts_with("ms-settings:") { return; }
    let _ = Command::new("cmd").args(["/C", "start", "", uri]).creation_flags(NO_WINDOW).spawn();
}
