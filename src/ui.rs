use std::{
    collections::{HashMap, VecDeque},
    iter::FromIterator,
    sync::{Arc, Mutex},
};

use sciter::Value;

use hbb_common::{
    allow_err,
    config::{LocalConfig, PeerConfig},
    log,
};

#[cfg(not(any(feature = "flutter", feature = "cli")))]
use crate::ui_session_interface::Session;
use crate::{common::get_app_name, ipc, ui_interface::*};

mod cm;
#[cfg(feature = "inline")]
pub mod inline;
pub mod remote;

#[allow(dead_code)]
type Status = (i32, bool, i64, String);

lazy_static::lazy_static! {
    // stupid workaround for https://sciter.com/forums/topic/crash-on-latest-tis-mac-sdk-sometimes/
    static ref STUPID_VALUES: Mutex<Vec<Arc<Vec<Value>>>> = Default::default();
}

#[cfg(not(any(feature = "flutter", feature = "cli")))]
lazy_static::lazy_static! {
    pub static ref CUR_SESSION: Arc<Mutex<Option<Session<remote::SciterHandler>>>> = Default::default();
}

lazy_static::lazy_static! {
    pub static ref CCTV_QUEUE: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
}

lazy_static::lazy_static! {
    pub static ref MAIN_HWND: Mutex<Option<isize>> = Default::default();
}

struct UIHostHandler;

pub fn start(args: &mut [String]) {
    #[cfg(target_os = "macos")]
    crate::platform::delegate::show_dock();
    #[cfg(all(target_os = "linux", feature = "inline"))]
    {
        let app_dir = std::env::var("APPDIR").unwrap_or("".to_string());
        let mut so_path = "/usr/share/rustdesk/libsciter-gtk.so".to_owned();
        for (prefix, dir) in [
            ("", "/usr"),
            ("", "/app"),
            (&app_dir, "/usr"),
            (&app_dir, "/app"),
        ]
        .iter()
        {
            let path = format!("{prefix}{dir}/share/rustdesk/libsciter-gtk.so");
            if std::path::Path::new(&path).exists() {
                so_path = path;
                break;
            }
        }
        sciter::set_library(&so_path).ok();
    }
    #[cfg(windows)]
    // Check if there is a sciter.dll nearby.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let sciter_dll_path = parent.join("sciter.dll");
            if sciter_dll_path.exists() {
                // Try to set the sciter dll.
                let p = sciter_dll_path.to_string_lossy().to_string();
                log::debug!("Found dll:{}, \n {:?}", p, sciter::set_library(&p));
            }
        }
    }
    // https://github.com/c-smile/sciter-sdk/blob/master/include/sciter-x-types.h
    // https://github.com/rustdesk/rustdesk/issues/132#issuecomment-886069737
    #[cfg(windows)]
    allow_err!(sciter::set_options(sciter::RuntimeOptions::GfxLayer(
        sciter::GFX_LAYER::WARP
    )));
    use sciter::SCRIPT_RUNTIME_FEATURES::*;
    allow_err!(sciter::set_options(sciter::RuntimeOptions::ScriptFeatures(
        ALLOW_FILE_IO as u8 | ALLOW_SOCKET_IO as u8 | ALLOW_EVAL as u8 | ALLOW_SYSINFO as u8
    )));
    let mut frame = sciter::WindowBuilder::main_window().create();

    let mut parent_hwnd = None;
    for i in 0..args.len() {
        if args[i] == "--parent-hwnd" && i + 1 < args.len() {
            if let Ok(val) = args[i + 1].parse::<isize>() {
                parent_hwnd = Some(val as winapi::shared::windef::HWND);
            }
        }
    }

    if parent_hwnd.is_none() {
        *MAIN_HWND.lock().unwrap() = Some(frame.get_hwnd() as isize);
    } else {
        #[cfg(windows)]
        if let Some(parent_hw) = parent_hwnd {
            let hwnd = frame.get_hwnd() as winapi::shared::windef::HWND;
            unsafe {
                let mut style = winapi::um::winuser::GetWindowLongW(hwnd, winapi::um::winuser::GWL_STYLE);
                style &= !((winapi::um::winuser::WS_CAPTION
                    | winapi::um::winuser::WS_THICKFRAME
                    | winapi::um::winuser::WS_MINIMIZEBOX
                    | winapi::um::winuser::WS_MAXIMIZEBOX
                    | winapi::um::winuser::WS_SYSMENU) as i32);
                style |= winapi::um::winuser::WS_CHILD as i32;
                winapi::um::winuser::SetWindowLongW(hwnd, winapi::um::winuser::GWL_STYLE, style);
                winapi::um::winuser::SetParent(hwnd, parent_hw);
                winapi::um::winuser::SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    0, 0, 0, 0,
                    winapi::um::winuser::SWP_NOMOVE | winapi::um::winuser::SWP_NOSIZE | winapi::um::winuser::SWP_NOZORDER | winapi::um::winuser::SWP_FRAMECHANGED,
                );
                winapi::um::winuser::ShowWindow(hwnd, winapi::um::winuser::SW_SHOW);
            }
        }
    }
    #[cfg(windows)]
    allow_err!(sciter::set_options(sciter::RuntimeOptions::UxTheming(true)));
    frame.set_title(&crate::get_app_name());
    #[cfg(target_os = "macos")]
    crate::platform::delegate::make_menubar(frame.get_host(), args.is_empty());
    #[cfg(windows)]
    crate::platform::try_set_window_foreground(frame.get_hwnd() as _);
    let page;
    if args.len() > 1 && args[0] == "--play" {
        args[0] = "--connect".to_owned();
        let path: std::path::PathBuf = (&args[1]).into();
        let id = path
            .file_stem()
            .map(|p| p.to_str().unwrap_or(""))
            .unwrap_or("")
            .to_owned();
        args[1] = id;
    }
    if args.is_empty() {
        std::thread::spawn(move || check_zombie());
        crate::common::check_software_update();
        frame.event_handler(UI {});
        frame.sciter_handler(UIHostHandler {});
        // Register native-remote behavior for CCTV embedded video
        frame.register_behavior("native-remote", move || {
            let id = CCTV_QUEUE.lock().unwrap().pop_front().unwrap_or_default();
            log::info!("[CCTV] creating session for peer: {}", id);
            let handler = remote::SciterSession::new(
                "--connect".to_string(),
                id,
                "".to_string(),
                vec![],
            );
            Box::new(handler)
        });
        page = "index.html";
        // Start pulse audio local server.
        #[cfg(target_os = "linux")]
        std::thread::spawn(crate::ipc::start_pa);
    } else if args[0] == "--install" {
        frame.event_handler(UI {});
        frame.sciter_handler(UIHostHandler {});
        page = "install.html";
    } else if args[0] == "--cm" {
        frame.register_behavior("connection-manager", move || {
            Box::new(cm::SciterConnectionManager::new())
        });
        page = "cm.html";
        *cm::HIDE_CM.lock().unwrap() = crate::ipc::get_config("hide_cm")
            .ok()
            .flatten()
            .unwrap_or_default()
            == "true";
    } else if (args[0] == "--connect"
        || args[0] == "--file-transfer"
        || args[0] == "--port-forward"
        || args[0] == "--rdp")
        && args.len() > 1
    {
        #[cfg(windows)]
        if parent_hwnd.is_none() {
            let hw = frame.get_host().get_hwnd();
            crate::platform::windows::enable_lowlevel_keyboard(hw as _);
        }
        let mut iter = args.iter();
        let Some(cmd) = iter.next() else {
            log::error!("Failed to get cmd arg");
            return;
        };
        let cmd = cmd.to_owned();
        let Some(id) = iter.next() else {
            log::error!("Failed to get id arg");
            return;
        };
        let id = id.to_owned();
        let pass = iter.next().unwrap_or(&"".to_owned()).clone();
        let args: Vec<String> = iter.map(|x| x.clone()).collect();
        frame.set_title(&id);
        frame.register_behavior("native-remote", move || {
            let handler =
                remote::SciterSession::new(cmd.clone(), id.clone(), pass.clone(), args.clone());
            #[cfg(not(any(feature = "flutter", feature = "cli")))]
            {
                *CUR_SESSION.lock().unwrap() = Some(handler.inner());
            }
            Box::new(handler)
        });
        page = "remote.html";
    } else {
        log::error!("Wrong command: {:?}", args);
        return;
    }
    #[cfg(feature = "inline")]
    {
        let html = if page == "index.html" {
            inline::get_index()
        } else if page == "cm.html" {
            inline::get_cm()
        } else if page == "install.html" {
            inline::get_install()
        } else {
            inline::get_remote()
        };
        frame.load_html(html.as_bytes(), Some(page));
    }
    #[cfg(not(feature = "inline"))]
    frame.load_file(&format!(
        "file://{}/src/ui/{}",
        std::env::current_dir()
            .map(|c| c.display().to_string())
            .unwrap_or("".to_owned()),
        page
    ));
    let hide_cm = *cm::HIDE_CM.lock().unwrap();
    if !args.is_empty() && args[0] == "--cm" && hide_cm {
        // run_app calls expand(show) + run_loop, we use collapse(hide) + run_loop instead to create a hidden window
        frame.collapse(true);
        frame.run_loop();
        return;
    }
    frame.run_app();
}

struct UI {}

impl UI {
    fn recent_sessions_updated(&self) -> bool {
        recent_sessions_updated()
    }

    fn get_id(&self) -> String {
        ipc::get_id()
    }

    fn temporary_password(&mut self) -> String {
        temporary_password()
    }

    fn update_temporary_password(&self) {
        update_temporary_password()
    }

    fn set_permanent_password(&self, password: String) {
        let _ = set_permanent_password_with_result(password);
    }

    fn is_local_permanent_password_set(&self) -> bool {
        is_local_permanent_password_set()
    }

    fn is_permanent_password_set(&self) -> bool {
        is_permanent_password_set()
    }

    fn get_remote_id(&mut self) -> String {
        LocalConfig::get_remote_id()
    }

    fn set_remote_id(&mut self, id: String) {
        LocalConfig::set_remote_id(&id);
    }

    fn goto_install(&mut self) {
        goto_install();
    }

    fn install_me(&mut self, _options: String, _path: String) {
        install_me(_options, _path, false, false);
    }

    fn update_me(&self, _path: String) {
        update_me(_path);
    }

    fn run_without_install(&self) {
        run_without_install();
    }

    fn show_run_without_install(&self) -> bool {
        show_run_without_install()
    }

    fn get_license(&self) -> String {
        get_license()
    }

    fn get_option(&self, key: String) -> String {
        get_option(key)
    }

    fn get_local_option(&self, key: String) -> String {
        get_local_option(key)
    }

    fn set_local_option(&self, key: String, value: String) {
        set_local_option(key, value);
    }

    fn peer_has_password(&self, id: String) -> bool {
        peer_has_password(id)
    }

    fn forget_password(&self, id: String) {
        forget_password(id)
    }

    fn get_peer_option(&self, id: String, name: String) -> String {
        get_peer_option(id, name)
    }

    fn set_peer_option(&self, id: String, name: String, value: String) {
        set_peer_option(id, name, value)
    }

    fn using_public_server(&self) -> bool {
        crate::using_public_server()
    }

    fn is_incoming_only(&self) -> bool {
        hbb_common::config::is_incoming_only()
    }

    pub fn is_outgoing_only(&self) -> bool {
        hbb_common::config::is_outgoing_only()
    }

    pub fn is_custom_client(&self) -> bool {
        crate::common::is_custom_client()
    }

    pub fn is_disable_settings(&self) -> bool {
        hbb_common::config::is_disable_settings()
    }

    pub fn is_disable_account(&self) -> bool {
        hbb_common::config::is_disable_account()
    }

    pub fn is_disable_installation(&self) -> bool {
        hbb_common::config::is_disable_installation()
    }

    pub fn is_disable_ab(&self) -> bool {
        hbb_common::config::is_disable_ab()
    }

    fn get_options(&self) -> Value {
        let hashmap: HashMap<String, String> =
            serde_json::from_str(&get_options()).unwrap_or_default();
        let mut m = Value::map();
        for (k, v) in hashmap {
            m.set_item(k, v);
        }
        m
    }

    fn test_if_valid_server(&self, host: String, test_with_proxy: bool) -> String {
        test_if_valid_server(host, test_with_proxy)
    }

    fn get_sound_inputs(&self) -> Value {
        Value::from_iter(get_sound_inputs())
    }

    fn set_options(&self, v: Value) {
        let mut m = HashMap::new();
        for (k, v) in v.items() {
            if let Some(k) = k.as_string() {
                if let Some(v) = v.as_string() {
                    if !v.is_empty() {
                        m.insert(k, v);
                    }
                }
            }
        }
        set_options(m);
    }

    fn set_option(&self, key: String, value: String) {
        set_option(key, value);
    }

    fn install_path(&mut self) -> String {
        install_path()
    }

    fn install_options(&self) -> String {
        install_options()
    }

    fn get_socks(&self) -> Value {
        Value::from_iter(get_socks())
    }

    fn set_socks(&self, proxy: String, username: String, password: String) {
        set_socks(proxy, username, password)
    }

    fn is_installed(&self) -> bool {
        is_installed()
    }

    fn get_supported_privacy_mode_impls(&self) -> String {
        serde_json::to_string(&crate::privacy_mode::get_supported_privacy_mode_impl())
            .unwrap_or_default()
    }

    fn is_root(&self) -> bool {
        is_root()
    }

    fn is_release(&self) -> bool {
        #[cfg(not(debug_assertions))]
        return true;
        #[cfg(debug_assertions)]
        return false;
    }

    fn is_share_rdp(&self) -> bool {
        is_share_rdp()
    }

    fn set_share_rdp(&self, _enable: bool) {
        set_share_rdp(_enable);
    }

    fn is_installed_lower_version(&self) -> bool {
        is_installed_lower_version()
    }

    fn closing(&mut self, x: i32, y: i32, w: i32, h: i32) {
        crate::server::input_service::fix_key_down_timeout_at_exit();
        LocalConfig::set_size(x, y, w, h);
    }

    fn get_size(&mut self) -> Value {
        let s = LocalConfig::get_size();
        let mut v = Vec::new();
        v.push(s.0);
        v.push(s.1);
        v.push(s.2);
        v.push(s.3);
        Value::from_iter(v)
    }

    fn get_mouse_time(&self) -> f64 {
        get_mouse_time()
    }

    fn check_mouse_time(&self) {
        check_mouse_time()
    }

    fn get_connect_status(&mut self) -> Value {
        let mut v = Value::array(0);
        let x = get_connect_status();
        v.push(x.status_num);
        v.push(x.key_confirmed);
        v.push(x.id);
        v
    }

    #[inline]
    fn get_peer_value(id: String, p: PeerConfig) -> Value {
        let values = vec![
            id,
            p.info.username.clone(),
            p.info.hostname.clone(),
            p.info.platform.clone(),
            p.options.get("alias").unwrap_or(&"".to_owned()).to_owned(),
        ];
        Value::from_iter(values)
    }

    fn get_peer(&self, id: String) -> Value {
        let c = get_peer(id.clone());
        Self::get_peer_value(id, c)
    }

    fn get_fav(&self) -> Value {
        Value::from_iter(get_fav())
    }

    fn store_fav(&self, fav: Value) {
        let mut tmp = vec![];
        fav.values().for_each(|v| {
            if let Some(v) = v.as_string() {
                if !v.is_empty() {
                    tmp.push(v);
                }
            }
        });
        store_fav(tmp);
    }

    fn get_recent_sessions(&mut self) -> Value {
        // to-do: limit number of recent sessions, and remove old peer file
        let peers: Vec<Value> = PeerConfig::peers(None)
            .drain(..)
            .map(|p| Self::get_peer_value(p.0, p.2))
            .collect();
        Value::from_iter(peers)
    }

    fn get_icon(&mut self) -> String {
        get_icon()
    }

    fn remove_peer(&mut self, id: String) {
        PeerConfig::remove(&id);
    }

    fn add_peer(&mut self, id: String) {
        add_peer(id)
    }

    fn remove_discovered(&mut self, id: String) {
        remove_discovered(id);
    }

    fn send_wol(&mut self, id: String) {
        crate::lan::send_wol(id)
    }

    fn new_remote(&mut self, id: String, remote_type: String, force_relay: bool) {
        new_remote(id, remote_type, force_relay)
    }

    fn new_file_transfer_auto(&mut self, id: String, password: String, folder: String) {
        crate::ui_interface::new_file_transfer_auto(id, password, folder)
    }

    fn new_remote_cctv(&mut self, id: String, remote_type: String, index: i32, total: i32) {
        new_remote_cctv(id, remote_type, index, total)
    }

    fn update_cctv_child_pos(&mut self, peer_id: String, x: i32, y: i32, w: i32, h: i32) {
        #[cfg(windows)]
        {
            let main_hwnd = match *MAIN_HWND.lock().unwrap() {
                Some(h) => h as winapi::shared::windef::HWND,
                None => return,
            };
            
            struct EnumData {
                target_title: String,
                found_hwnd: Option<winapi::shared::windef::HWND>,
            }
            
            unsafe extern "system" fn enum_child_proc(
                hwnd: winapi::shared::windef::HWND,
                lparam: winapi::shared::minwindef::LPARAM,
            ) -> winapi::shared::minwindef::BOOL {
                let data = &mut *(lparam as *mut EnumData);
                let mut buf = [0u16; 512];
                let len = winapi::um::winuser::GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
                if len > 0 {
                    let title = String::from_utf16_lossy(&buf[..len as usize]);
                    let clean_title = title.replace(" ", "");
                    let clean_target = data.target_title.replace(" ", "");
                    if clean_title == clean_target {
                        data.found_hwnd = Some(hwnd);
                        return winapi::shared::minwindef::FALSE;
                    }
                }
                winapi::shared::minwindef::TRUE
            }
            
            let mut data = EnumData {
                target_title: peer_id,
                found_hwnd: None,
            };
            
            unsafe {
                winapi::um::winuser::EnumChildWindows(
                    main_hwnd,
                    Some(enum_child_proc),
                    &mut data as *mut _ as winapi::shared::minwindef::LPARAM,
                );
                
                if let Some(child_hwnd) = data.found_hwnd {
                    winapi::um::winuser::MoveWindow(
                        child_hwnd,
                        x,
                        y,
                        w,
                        h,
                        winapi::shared::minwindef::TRUE,
                    );
                }
            }
        }
    }

    fn close_all_cctv(&mut self) {
        close_all_cctv();
    }

    fn push_cctv_peer(&mut self, id: String) {
        CCTV_QUEUE.lock().unwrap().push_back(id);
    }

    fn clear_cctv_queue(&mut self) {
        CCTV_QUEUE.lock().unwrap().clear();
    }

    fn auth_login(&mut self, user_id: String) -> String {
        let rt = hbb_common::tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            use hbb_common::tokio::net::TcpStream;
            use tokio_tungstenite::tungstenite::Message;
            use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
            use hbb_common::futures_util::{SinkExt, StreamExt};

            let mut host = hbb_common::config::Config::get_option("custom-rendezvous-server");
            if host.is_empty() {
                host = "127.0.0.1".to_string();
            }
            let host = host.split(':').next().unwrap_or("127.0.0.1").trim().to_string();
            let url = format!("ws://{}:3000/ws", host);
            match hbb_common::tokio::time::timeout(std::time::Duration::from_secs(3), connect_async(url.clone())).await {
                Ok(Ok((mut ws_stream, _))) => {
                    let req = serde_json::json!({ "user_id": user_id });
                    if ws_stream.send(Message::Text(req.to_string().into())).await.is_err() {
                        return "Failed to send login request".to_string();
                    }

                    if let Some(Ok(Message::Text(msg))) = ws_stream.next().await {
                        let resp: serde_json::Value = serde_json::from_str(msg.to_string().as_str()).unwrap_or_default();
                        if resp["status"] != "OK" {
                            return resp["message"].as_str().unwrap_or("Login failed").to_string();
                        }
                    } else {
                        return "No response from auth server".to_string();
                    }

                    // Connected and verified, spawn background thread to listen for EXPIRED
                    std::thread::spawn(move || {
                        let bg_rt = hbb_common::tokio::runtime::Runtime::new().unwrap();
                        bg_rt.block_on(async move {
                            while let Some(msg) = ws_stream.next().await {
                                if let Ok(Message::Text(text)) = msg {
                                    let resp: serde_json::Value = serde_json::from_str(text.to_string().as_str()).unwrap_or_default();
                                    if resp["status"] == "ERROR" {
                                        #[cfg(windows)]
                                        unsafe {
                                            use std::os::windows::ffi::OsStrExt;
                                            let msg_str = resp["message"].as_str().unwrap_or("Session terminated");
                                            let wide: Vec<u16> = std::ffi::OsStr::new(msg_str).encode_wide().chain(std::iter::once(0)).collect();
                                            let title: Vec<u16> = std::ffi::OsStr::new("Error").encode_wide().chain(std::iter::once(0)).collect();
                                            winapi::um::winuser::MessageBoxW(std::ptr::null_mut(), wide.as_ptr(), title.as_ptr(), 0x10);
                                        }
                                        std::process::exit(0);
                                    }
                                }
                            }
                            // Auth server disconnected
                            #[cfg(windows)]
                            unsafe {
                                use std::os::windows::ffi::OsStrExt;
                                let wide: Vec<u16> = std::ffi::OsStr::new("Auth server disconnected").encode_wide().chain(std::iter::once(0)).collect();
                                let title: Vec<u16> = std::ffi::OsStr::new("Error").encode_wide().chain(std::iter::once(0)).collect();
                                winapi::um::winuser::MessageBoxW(std::ptr::null_mut(), wide.as_ptr(), title.as_ptr(), 0x10);
                            }
                            std::process::exit(0);
                        });
                    });

                    "OK".to_string()
                }
                Ok(Err(e)) => {
                    format!("Connection failed to {}: {}", url, e)
                }
                Err(_) => {
                    format!("Connection to {} timed out. Server might be down or firewall blocked.", url)
                }
            }
        })
    }

    fn is_process_trusted(&mut self, _prompt: bool) -> bool {
        is_process_trusted(_prompt)
    }

    fn is_can_screen_recording(&mut self, _prompt: bool) -> bool {
        is_can_screen_recording(_prompt)
    }

    fn is_installed_daemon(&mut self, _prompt: bool) -> bool {
        is_installed_daemon(_prompt)
    }

    fn get_error(&mut self) -> String {
        get_error()
    }

    fn is_login_wayland(&mut self) -> bool {
        is_login_wayland()
    }

    fn current_is_wayland(&mut self) -> bool {
        current_is_wayland()
    }

    fn get_software_update_url(&self) -> String {
        crate::SOFTWARE_UPDATE_URL.lock().unwrap().clone()
    }

    fn get_new_version(&self) -> String {
        get_new_version()
    }

    fn get_version(&self) -> String {
        get_version()
    }

    fn get_fingerprint(&self) -> String {
        get_fingerprint()
    }

    fn get_app_name(&self) -> String {
        get_app_name()
    }

    fn get_software_ext(&self) -> String {
        #[cfg(windows)]
        let p = "exe";
        #[cfg(target_os = "macos")]
        let p = "dmg";
        #[cfg(target_os = "linux")]
        let p = "deb";
        p.to_owned()
    }

    fn get_software_store_path(&self) -> String {
        let mut p = std::env::temp_dir();
        let name = crate::SOFTWARE_UPDATE_URL
            .lock()
            .unwrap()
            .split("/")
            .last()
            .map(|x| x.to_owned())
            .unwrap_or(crate::get_app_name());
        p.push(name);
        format!("{}.{}", p.to_string_lossy(), self.get_software_ext())
    }

    fn create_shortcut(&self, _id: String) {
        #[cfg(windows)]
        create_shortcut(_id)
    }

    fn discover(&self) {
        std::thread::spawn(move || {
            allow_err!(crate::lan::discover());
        });
    }

    fn get_lan_peers(&self) -> String {
        // let peers = get_lan_peers()
        //     .into_iter()
        //     .map(|mut peer| {
        //         (
        //             peer.remove("id").unwrap_or_default(),
        //             peer.remove("username").unwrap_or_default(),
        //             peer.remove("hostname").unwrap_or_default(),
        //             peer.remove("platform").unwrap_or_default(),
        //         )
        //     })
        //     .collect::<Vec<(String, String, String, String)>>();
        serde_json::to_string(&get_lan_peers()).unwrap_or_default()
    }

    fn get_uuid(&self) -> String {
        get_uuid()
    }

    fn open_url(&self, url: String) {
        #[cfg(windows)]
        let p = "explorer";
        #[cfg(target_os = "macos")]
        let p = "open";
        #[cfg(target_os = "linux")]
        let p = if std::path::Path::new("/usr/bin/firefox").exists() {
            "firefox"
        } else {
            "xdg-open"
        };
        allow_err!(std::process::Command::new(p).arg(url).spawn());
    }

    fn change_id(&self, id: String) {
        reset_async_job_status();
        let old_id = self.get_id();
        change_id_shared(id, old_id);
    }

    fn http_request(&self, url: String, method: String, body: Option<String>, header: String) {
        http_request(url, method, body, header)
    }

    fn post_request(&self, url: String, body: String, header: String) {
        post_request(url, body, header)
    }

    fn is_ok_change_id(&self) -> bool {
        hbb_common::machine_uid::get().is_ok()
    }

    fn get_async_job_status(&self) -> String {
        get_async_job_status()
    }

    fn get_http_status(&self, url: String) -> Option<String> {
        get_async_http_status(url)
    }

    fn t(&self, name: String) -> String {
        crate::client::translate(name)
    }

    fn is_xfce(&self) -> bool {
        crate::platform::is_xfce()
    }

    fn get_api_server(&self) -> String {
        get_api_server()
    }

    fn has_hwcodec(&self) -> bool {
        has_hwcodec()
    }

    fn has_vram(&self) -> bool {
        has_vram()
    }

    fn get_langs(&self) -> String {
        get_langs()
    }

    fn video_save_directory(&self, root: bool) -> String {
        video_save_directory(root)
    }

    fn handle_relay_id(&self, id: String) -> String {
        handle_relay_id(&id).to_owned()
    }

    fn get_login_device_info(&self) -> String {
        get_login_device_info_json()
    }

    fn support_remove_wallpaper(&self) -> bool {
        support_remove_wallpaper()
    }

    fn has_valid_2fa(&self) -> bool {
        has_valid_2fa()
    }

    fn generate2fa(&self) -> String {
        generate2fa()
    }

    pub fn verify2fa(&self, code: String) -> bool {
        verify2fa(code)
    }

    fn verify_login(&self, raw: String, id: String) -> bool {
        crate::verify_login(&raw, &id)
    }

    fn generate_2fa_img_src(&self, data: String) -> String {
        let v = qrcode_generator::to_png_to_vec(data, qrcode_generator::QrCodeEcc::Low, 128)
            .unwrap_or_default();
        let s = hbb_common::sodiumoxide::base64::encode(
            v,
            hbb_common::sodiumoxide::base64::Variant::Original,
        );
        format!("data:image/png;base64,{s}")
    }

    pub fn check_hwcodec(&self) {
        check_hwcodec()
    }

    fn is_option_fixed(&self, key: String) -> bool {
        crate::ui_interface::is_option_fixed(&key)
    }

    fn get_builtin_option(&self, key: String) -> String {
        crate::ui_interface::get_builtin_option(&key)
    }

    fn is_remote_modify_enabled_by_control_permissions(&self) -> String {
        match crate::ui_interface::is_remote_modify_enabled_by_control_permissions() {
            Some(true) => "true",
            Some(false) => "false",
            None => "",
        }
        .to_string()
    }
}

impl sciter::EventHandler for UI {
    sciter::dispatch_script_call! {
        fn t(String);
        fn get_api_server();
        fn is_xfce();
        fn using_public_server();
        fn is_custom_client();
        fn is_outgoing_only();
        fn is_incoming_only();
        fn is_disable_settings();
        fn is_disable_account();
        fn is_disable_installation();
        fn is_disable_ab();
        fn get_id();
        fn temporary_password();
        fn update_temporary_password();
        fn set_permanent_password(String);
        fn is_local_permanent_password_set();
        fn is_permanent_password_set();
        fn get_remote_id();
        fn set_remote_id(String);
        fn closing(i32, i32, i32, i32);
        fn get_size();
        fn new_remote(String, String, bool);
        fn new_file_transfer_auto(String, String, String);
        fn new_remote_cctv(String, String, i32, i32);
        fn update_cctv_child_pos(String, i32, i32, i32, i32);
        fn close_all_cctv();
        fn push_cctv_peer(String);
        fn clear_cctv_queue();
        fn auth_login(String);
        fn send_wol(String);
        fn remove_peer(String);
        fn add_peer(String);
        fn remove_discovered(String);
        fn get_connect_status();
        fn get_mouse_time();
        fn check_mouse_time();
        fn get_recent_sessions();
        fn get_peer(String);
        fn get_fav();
        fn store_fav(Value);
        fn recent_sessions_updated();
        fn get_icon();
        fn install_me(String, String);
        fn is_installed();
        fn get_supported_privacy_mode_impls();
        fn is_root();
        fn is_release();
        fn set_socks(String, String, String);
        fn get_socks();
        fn is_share_rdp();
        fn set_share_rdp(bool);
        fn is_installed_lower_version();
        fn install_path();
        fn install_options();
        fn goto_install();
        fn is_process_trusted(bool);
        fn is_can_screen_recording(bool);
        fn is_installed_daemon(bool);
        fn get_error();
        fn is_login_wayland();
        fn current_is_wayland();
        fn get_options();
        fn get_option(String);
        fn get_local_option(String);
        fn set_local_option(String, String);
        fn get_peer_option(String, String);
        fn peer_has_password(String);
        fn forget_password(String);
        fn set_peer_option(String, String, String);
        fn get_license();
        fn test_if_valid_server(String, bool);
        fn get_sound_inputs();
        fn set_options(Value);
        fn set_option(String, String);
        fn get_software_update_url();
        fn get_new_version();
        fn get_version();
        fn get_fingerprint();
        fn update_me(String);
        fn show_run_without_install();
        fn run_without_install();
        fn get_app_name();
        fn get_software_store_path();
        fn get_software_ext();
        fn open_url(String);
        fn change_id(String);
        fn get_async_job_status();
        fn post_request(String, String, String);
        fn is_ok_change_id();
        fn create_shortcut(String);
        fn discover();
        fn get_lan_peers();
        fn get_uuid();
        fn has_hwcodec();
        fn has_vram();
        fn get_langs();
        fn video_save_directory(bool);
        fn handle_relay_id(String);
        fn get_login_device_info();
        fn support_remove_wallpaper();
        fn has_valid_2fa();
        fn generate2fa();
        fn generate_2fa_img_src(String);
        fn verify2fa(String);
        fn check_hwcodec();
        fn verify_login(String, String);
        fn is_option_fixed(String);
        fn get_builtin_option(String);
        fn is_remote_modify_enabled_by_control_permissions();
    }
}

impl sciter::host::HostHandler for UIHostHandler {
    fn on_graphics_critical_failure(&mut self) {
        log::error!("Critical rendering error: e.g. DirectX gfx driver error. Most probably bad gfx drivers.");
    }
}

#[cfg(not(target_os = "linux"))]
fn get_sound_inputs() -> Vec<String> {
    let mut out = Vec::new();
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    if let Ok(devices) = host.devices() {
        for device in devices {
            if device.default_input_config().is_err() {
                continue;
            }
            if let Ok(name) = device.name() {
                out.push(name);
            }
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn get_sound_inputs() -> Vec<String> {
    crate::platform::linux::get_pa_sources()
        .drain(..)
        .map(|x| x.1)
        .collect()
}

// sacrifice some memory
pub fn value_crash_workaround(values: &[Value]) -> Arc<Vec<Value>> {
    let persist = Arc::new(values.to_vec());
    STUPID_VALUES.lock().unwrap().push(persist.clone());
    persist
}

pub fn get_icon() -> String {
    "data:image/png;base64,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".to_string()
}
