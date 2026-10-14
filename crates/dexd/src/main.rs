//! dexd [serve] [--bind ADDR] [--data-dir DIR] [--config-dir DIR] [--root DIR]… [--secrets auto|keyring|file]
//! dexd --local --data-dir DIR [--config-dir DIR] [--idle-exit SECS] [--background] [--wait-lock]
//!                                             the desktop app's own core (#150, src/local.rs)
//! dexd --version                             this build: {"commit", "commitTime"} (the shell's update check, #161)
//! dexd --desktop-data-dir IDENTIFIER          read-only desktop path query for development tooling
//! dexd pair [--data-dir DIR]                  one-time pairing code (10 min)
//! dexd devices list [--data-dir DIR]
//! dexd devices revoke <ID> [--data-dir DIR]
//! dexd map-refresh [--root DIR]…              refresh <root>/.dex/map.md skeleton (default: cwd)

use dex_core::Paths;
use dexd::auth::{Auth, CODE_TTL};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "kullanım:
  dexd [serve] [--bind 127.0.0.1:7717] [--data-dir DİZİN] [--config-dir DİZİN]
              [--root DİZİN]… [--secrets auto|keyring|file]
  dexd --local --data-dir DİZİN [--config-dir DİZİN] [--idle-exit SN] [--background]
  dexd --desktop-data-dir KİMLİK
  dexd pair [--data-dir DİZİN]
  dexd devices list [--data-dir DİZİN]
  dexd devices revoke <ID> [--data-dir DİZİN]
  dexd map-refresh [--root DİZİN]…";

struct Opts {
    words: Vec<String>,
    bind: SocketAddr,
    data_dir: PathBuf,
    config_dir: PathBuf,
    /// S4: folders clients may browse / put a team in (default `~/projects`).
    roots: Vec<PathBuf>,
    secrets: String,
    /// #150: `--local` and its knobs (`--idle-exit` = the no-window grace, for tests).
    local: bool,
    idle_exit: std::time::Duration,
    background: bool,
    wait_lock: bool,
}

fn parse() -> Result<Opts, String> {
    let (data, config) = default_dirs();
    let mut o = Opts {
        words: Vec::new(),
        bind: SocketAddr::from(([127, 0, 0, 1], dexd::DEFAULT_PORT)),
        data_dir: data,
        config_dir: config,
        roots: Vec::new(),
        secrets: "auto".into(),
        local: false,
        idle_exit: dexd::local::GRACE,
        background: false,
        wait_lock: false,
    };
    let mut config_set = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut val = || it.next().ok_or(format!("{a} bir değer bekliyor"));
        match a.as_str() {
            "--bind" => o.bind = val()?.parse().map_err(|e| format!("--bind: {e}"))?,
            "--data-dir" => o.data_dir = val()?.into(),
            "--config-dir" => {
                o.config_dir = val()?.into();
                config_set = true;
            }
            "--root" => o.roots.push(val()?.into()),
            "--secrets" => o.secrets = val()?,
            "--local" => o.local = true,
            "--idle-exit" => o.idle_exit = std::time::Duration::from_secs(val()?.parse().map_err(|e| format!("--idle-exit: {e}"))?),
            "--background" => o.background = true,
            "--wait-lock" => o.wait_lock = true,
            "-h" | "--help" => return Err(String::new()),
            _ if a.starts_with("--") => return Err(format!("bilinmeyen seçenek: {a}")),
            _ => o.words.push(a),
        }
    }
    // One flag is enough for an isolated instance (tests, a second server).
    if !config_set && std::env::args().any(|a| a == "--data-dir") {
        o.config_dir = o.data_dir.clone();
    }
    Ok(o)
}

/// Its own folder, never the desktop app's: two cores on one `dex.db` would close each other's
/// delegations at startup.
fn default_dirs() -> (PathBuf, PathBuf) {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"));
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    let dir = base.unwrap_or_else(|| PathBuf::from(".")).join("dexd");
    (dir.clone(), dir)
}

fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some("--desktop-data-dir") {
        let result = std::env::args().nth(2)
            .ok_or("--desktop-data-dir bir kimlik bekliyor")
            .and_then(|id| dex_core::desktop_storage::resolve(&id));
        return match result {
            Ok(paths) => {
                println!("{}", paths.data_dir.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("dexd: {e}");
                ExitCode::from(2)
            }
        };
    }
    if std::env::args().nth(1).as_deref() == Some("--version") {
        let t = env!("DEX_COMMIT_TIME").parse::<i64>().unwrap_or(0);
        println!("{}", serde_json::json!({ "commit": env!("DEX_COMMIT"), "commitTime": t }));
        return ExitCode::SUCCESS;
    }
    let o = match parse() {
        Ok(o) => o,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("dexd: {e}");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let words: Vec<&str> = o.words.iter().map(String::as_str).collect();
    let r = match words.as_slice() {
        [] if o.local => local(o),
        _ if o.local => Err(format!("--local bir komut almaz
{USAGE}")),
        [] | ["serve"] => serve(o),
        ["pair"] => pair(&o),
        ["devices", "list"] | ["devices"] => devices(&o),
        ["devices", "revoke", id] => revoke(&o, id),
        ["map-refresh"] => map_refresh(&o),
        _ => Err(format!("bilinmeyen komut\n{USAGE}")),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("dexd: {e}");
            ExitCode::FAILURE
        }
    }
}

fn auth(o: &Opts) -> Result<Auth, String> {
    Auth::open(&o.data_dir.join("dexd.db"))
}

fn pair(o: &Opts) -> Result<(), String> {
    let code = auth(o)?.new_pair_code(CODE_TTL)?;
    println!("{code}");
    eprintln!("Eşleme kodu {} dk geçerli, tek kullanımlık.", CODE_TTL.as_secs() / 60);
    Ok(())
}

fn devices(o: &Opts) -> Result<(), String> {
    for d in auth(o)?.list()? {
        let seen = d.last_seen.map_or("hiç".to_string(), |t| t.to_string());
        println!("{}\t{}\teklendi={}\tson={}", d.id, d.name, d.created_at, seen);
    }
    Ok(())
}

fn revoke(o: &Opts, id: &str) -> Result<(), String> {
    if !auth(o)?.revoke(id)? {
        return Err(format!("böyle bir cihaz yok: {id}"));
    }
    println!("iptal edildi: {id} (açık bağlantısı {} sn içinde kapanır)", dexd::server::RECHECK.as_secs());
    Ok(())
}

/// Plan #99 mapgen: elle tetikleme — `<root>/.dex/map.md` iskeletini tazeler.
/// `--root` verilmezse çalışılan dizin; Core/DB gerekmez, salt dosya işi.
fn map_refresh(o: &Opts) -> Result<(), String> {
    let roots = if o.roots.is_empty() {
        vec![std::env::current_dir().map_err(|e| e.to_string())?]
    } else {
        o.roots.clone()
    };
    for root in &roots {
        let started = std::time::Instant::now();
        let out = dex_core::repomap::refresh(root)?;
        println!(
            "{}: {} dosya, {} simge, {} bayt, {} ms",
            out.path.display(),
            out.files,
            out.symbols,
            out.bytes,
            started.elapsed().as_millis()
        );
    }
    Ok(())
}

/// #150: never the `dexd` default dir — the app's, given by the shell that starts it (one
/// `dex.db`, one `dex.lock`). Secrets: the OS keyring, as in the app.
fn local(o: Opts) -> Result<(), String> {
    if !std::env::args().any(|a| a == "--data-dir") {
        return Err("--local --data-dir ister (uygulamanın veri dizini)".into());
    }
    dexd::local::run(dexd::local::Opts {
        paths: Paths { data_dir: o.data_dir, config_dir: o.config_dir },
        grace: o.idle_exit,
        wait_lock: o.wait_lock,
        background: o.background,
        args: std::env::args().skip(1).collect(),
    })
}

fn serve(o: Opts) -> Result<(), String> {
    dex_core::diagnostics::init(&o.data_dir.join("logs"))?;
    use dex_core::secrets::{self, Store};
    // S4: headless Linux has no Secret Service; provider tokens then go to 0600 files.
    let files = || Store::Files(o.data_dir.join("secrets"));
    let store = match o.secrets.as_str() {
        "keyring" => Store::Keyring,
        "file" => files(),
        "auto" if secrets::keyring_works() => Store::Keyring,
        "auto" => files(),
        s => return Err(format!("--secrets: auto, keyring ya da file olmalı (verilen: {s})")),
    };
    secrets::use_store(store)?;
    eprintln!("dexd: sırlar: {}", secrets::describe());
    let roots = if o.roots.is_empty() { vec![dexd::fs::default_root()] } else { o.roots.clone() };
    let roots = dexd::fs::Roots::new(&roots)?;
    for r in roots.paths() {
        eprintln!("dexd: izinli klasör: {}", r.display());
    }
    let paths = Paths { data_dir: o.data_dir, config_dir: o.config_dir };
    let srv = dexd::Server::start(paths, roots)?;
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let r = rt.block_on(async {
        let listener = tokio::net::TcpListener::bind(o.bind).await.map_err(|e| format!("{}: {e}", o.bind))?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        if !addr.ip().is_loopback() {
            eprintln!("dexd: UYARI: {addr} döngü dışı bir adres; trafik şifresiz (ws://) — yalnız tailnet gibi şifreli bir ağda aç.");
        }
        // Machine-readable first line (the port when `--bind` asked for :0).
        println!("dexd: ws://{addr}");
        tokio::select! {
            _ = dexd::serve(listener, srv.clone()) => {}
            _ = tokio::signal::ctrl_c() => eprintln!("dexd: kapanıyor"),
        }
        Ok::<_, String>(())
    });
    srv.core.shutdown();
    r
}
