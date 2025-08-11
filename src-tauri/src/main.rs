// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// use serde_json::json;
mod background_thread;
mod commands;
mod helpers;
mod injector;
mod ipc;
mod query;
mod rpcs;
mod samp;

#[path = "nativestorage/lib.rs"]
mod nativestorage;

#[path = "deeplink/lib.rs"]
#[cfg(target_os = "windows")]
mod deeplink;

use std::env;
use std::process::exit;
use std::sync::Mutex;

use background_thread::initialize_background_thread;
use gumdrop::Options;
use injector::run_samp;
use log::{error, info, LevelFilter};
// use std::io::Read;
use std::fs;
use tauri::api::path::app_data_dir;
use tauri::Manager;
use tauri::PhysicalSize;

#[derive(Debug, Options)]
struct CliArgs {
    #[options(no_short, help = "print help message")]
    help: bool,

    #[options(help = "target server IP address")]
    host: Option<String>,

    #[options(help = "target server port")]
    port: Option<i32>,

    #[options(help = "target server password")]
    password: Option<String>,

    #[options(help = "nickname to join server with")]
    name: Option<String>,

    #[options(help = "game path to use for both game executable and samp.dll")]
    gamepath: Option<String>,
}

static URI_SCHEME_VALUE: Mutex<String> = Mutex::new(String::new());

#[tauri::command]
async fn get_uri_scheme_value() -> String {
    URI_SCHEME_VALUE.lock().unwrap().clone()
}

#[tokio::main]
async fn main() {
    // let mut f =
    //     std::fs::File::open("D:\\Projects\\open.mp\\Launcher-tauri\\omp-launcher\\omp-client.dll")
    //         .unwrap();
    // let mut contents = Vec::<u8>::new();
    // f.read_to_end(&mut contents).unwrap();
    // let digest = md5::compute(contents.as_slice());
    // println!("{:x}", digest);

    #[cfg(windows)]
    {
        deeplink::prepare("mp.open.launcher");
    }

    if let Err(e) = simple_logging::log_to_file("omp-launcher.log", LevelFilter::Info) {
        eprintln!("Failed to initialize logging to file: {}", e);
        simple_logging::log_to_stderr(LevelFilter::Info);
    }

    #[cfg(windows)]
    {
        #[cfg(not(debug_assertions))]
        {
            use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
            let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
        }
    }

    let raw_args: Vec<String> = env::args().collect();
    let parse_args_result = CliArgs::parse_args_default::<String>(&raw_args[1..]);
    match parse_args_result {
        Ok(args) => {
            if args.help {
                println!(
                    "Open Multiplayer Launcher

Usage: {} [OPTIONS]

Options:
      --help
  -h, --host <HOST>          Server IP
  -p, --port <PORT>          Server port
  -P, --password <PASSWORD>  Server password
  -n, --name <NAME>          Nickname
  -g, --gamepath <GAMEPATH>  Game path
                ",
                    raw_args[0]
                );
                exit(0)
            }

            if args.host.is_some() && args.name.is_some() && args.port.is_some() {
                if let Some(gamepath) = &args.gamepath {
                    if !gamepath.is_empty() {
                        let password: String = args.password.unwrap_or_default();
                        
                        let name = args.name.as_deref().unwrap();
                        let host = args.host.as_deref().unwrap();
                        let port = args.port.unwrap();
                        let gamepath_str = gamepath.as_str();
                        let samp_dll_path = format!("{}/samp.dll", gamepath);
                        
                        let data_dir = match dirs_next::data_local_dir() {
                            Some(dir) => dir,
                            None => {
                                error!("Could not determine local data directory");
                                exit(1);
                            }
                        };
                        
                        let data_dir_str = match data_dir.to_str() {
                            Some(s) => s,
                            None => {
                                error!("Local data directory path contains invalid characters");
                                exit(1);
                            }
                        };
                        
                        let omp_client_path = format!("{}/mp.open.launcher/omp/omp-client.dll", data_dir_str);
                        
                        let _ = run_samp(
                            name,
                            host,
                            port,
                            gamepath_str,
                            &samp_dll_path,
                            &omp_client_path,
                            &password,
                            true,
                        )
                        .await;
                        info!("Attempted to run the game from command line");
                        exit(0)
                    } else {
                        println!("You must provide game path using --game or -g. Read more about arguments in --help");
                        info!("You must provide game path using --game or -g. Read more about arguments in --help");
                        exit(0)
                    }
                } else {
                    println!("You must provide game path using --game or -g. Read more about arguments in --help");
                    info!("You must provide game path using --game or -g. Read more about arguments in --help");
                    exit(0)
                }
            }
        }
        Err(e) => {
            if raw_args[1].contains("omp://") || raw_args[1].contains("samp://") {
                let mut uri_scheme_value = URI_SCHEME_VALUE.lock().unwrap();
                *uri_scheme_value = String::from(raw_args[1].as_str());
            } else {
                info!("Unknown argument has been passed: {}", e);
            }
        }
    };

    #[cfg(windows)]
    {
        #[cfg(not(debug_assertions))]
        {
            use windows::Win32::System::Console::FreeConsole;
            let _ = unsafe { FreeConsole() };
        }
    }

    initialize_background_thread();
    std::thread::spawn(move || {
        match actix_rt::Runtime::new() {
            Ok(rt) => {
                if let Err(e) = rt.block_on(rpcs::initialize_rpc()) {
                    error!("RPC server failed to start: {}", e);
                }
            }
            Err(e) => {
                error!("Failed to create actix runtime: {}", e);
            }
        }
    });

    match tauri::Builder::default()
        .plugin(tauri_plugin_upload::init())
        .setup(|app| {
            let handle = app.handle();
            let main_window = match app.get_window("main") {
                Some(window) => window,
                None => {
                    error!("Failed to get main window");
                    return Err("Main window not found".into());
                }
            };
            
            if let Err(e) = main_window.set_min_size(Some(PhysicalSize::new(1000, 700))) {
                error!("Failed to set minimum window size: {}", e);
            }

            let config = handle.config();
            if let Some(path) = app_data_dir(&config) {
                if let Err(e) = fs::create_dir_all(&path) {
                    println!("Failed to create app data directory: {}", e);
                }
            }

            #[cfg(windows)]
            {
                let handle = app.handle();
                let handle2 = app.handle();

                if let Err(e) = deeplink::register("omp", move |request| {
                    dbg!(&request);
                    if let Ok(mut uri_scheme_value) = URI_SCHEME_VALUE.lock() {
                        *uri_scheme_value = String::from(request.as_str());
                        if let Err(emit_err) = handle.emit_all("scheme-request-received", request) {
                            error!("Failed to emit scheme request: {}", emit_err);
                        }
                    }
                }) {
                    error!("Failed to register omp deeplink handler: {}", e);
                }

                if let Err(e) = deeplink::register("samp", move |request| {
                    dbg!(&request);
                    if let Ok(mut uri_scheme_value) = URI_SCHEME_VALUE.lock() {
                        (*uri_scheme_value).clone_from(&request);
                        *uri_scheme_value = String::from(request.as_str());
                        if let Err(emit_err) = handle2.emit_all("scheme-request-received", request) {
                            error!("Failed to emit scheme request: {}", emit_err);
                        }
                    }
                }) {
                    error!("Failed to register samp deeplink handler: {}", e);
                }
            }

            ipc::listen_for_ipc(handle);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_uri_scheme_value,
            commands::inject,
            commands::get_gtasa_path_from_samp,
            commands::get_nickname_from_samp,
            commands::get_samp_favorite_list,
            commands::rerun_as_admin,
            commands::resolve_hostname,
            commands::is_process_alive,
            commands::log,
            query::query_server,
            ipc::send_message_to_game
        ])
        .run(tauri::generate_context!())
    {
        Ok(_) => {}
        Err(e) => {
            error!("[main.rs] Running tauri instance failed: {}", e.to_string());
        }
    };
}
