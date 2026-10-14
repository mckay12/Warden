mod blocklist_refresh;
mod browser_policy;
mod db;
mod db_check;
mod dns_enforce;
mod dns_filter;
mod emergency_unlock_logic;
mod extension_id;
mod firewall;
mod install;
mod lock_logic;
mod logging;
mod native_host_install;
mod native_messaging;
mod registry_policy;
mod service;
mod stats;
mod update_server;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str);

    match cmd {
        Some("install") => match install::install() {
            Ok(()) => println!("Warden service installed and started."),
            Err(e) => {
                eprintln!("Install failed: {e}");
                std::process::exit(1);
            }
        },
        Some("uninstall") => match install::uninstall() {
            Ok(()) => println!("Warden service stopped and removed."),
            Err(e) => {
                eprintln!("Uninstall failed: {e}");
                std::process::exit(1);
            }
        },
        Some("register-native-host") => match native_host_install::install() {
            Ok(()) => println!(
                "Native messaging host '{}' registered for the current user.",
                native_host_install::HOST_NAME
            ),
            Err(e) => {
                eprintln!("register-native-host failed: {e}");
                std::process::exit(1);
            }
        },
        Some("unregister-native-host") => match native_host_install::uninstall() {
            Ok(()) => println!("Native messaging host unregistered."),
            Err(e) => {
                eprintln!("unregister-native-host failed: {e}");
                std::process::exit(1);
            }
        },
        // Manual verification harness (Phase 5 exit criteria): opens the
        // real on-disk DB (creating it if missing), exercises CRUD across
        // every table, and prints row counts. `cargo test` covers the same
        // CRUD layer in isolation via an in-memory DB — this instead proves
        // it against the actual %ProgramData% path a real service run uses.
        Some("db-check") => match db_check::run() {
            Ok(()) => {}
            Err(e) => {
                eprintln!("db-check failed: {e}");
                std::process::exit(1);
            }
        },
        // Manual verification harness (Phase 13): forces a blocklist
        // refresh against the real DB right now, ignoring the interval
        // check, and prints what happened — the real running service
        // instead waits for `service_config.blocklist_refresh_interval_hours`
        // and does this silently (to `service.log`) on its own thread.
        Some("refresh-blocklist") => {
            let mut conn = match db::open() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("db open failed: {e}");
                    std::process::exit(1);
                }
            };
            // Force it regardless of the configured interval by clearing
            // last_refreshed_at first, so this command is actually useful
            // to run twice in a row while testing.
            if let Ok(Some(source)) =
                db::blocklist_source::get_by_url(&conn, blocklist_refresh::SOURCE_URL)
            {
                let _ = conn.execute(
                    "UPDATE blocklist_source SET last_refreshed_at = NULL WHERE id = ?1",
                    [source.id],
                );
            }
            match blocklist_refresh::maybe_refresh(&mut conn) {
                Ok(()) => println!("Blocklist refresh attempted — see service.log for the outcome."),
                Err(e) => {
                    eprintln!("refresh-blocklist failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        // Manual harness for the DNS filter. Without --enforce it runs in
        // the foreground on a loopback port and does NOT touch system DNS,
        // so it can be tried with a raw DNS client. With --enforce it also
        // points the machine's DNS at the filter (port 53, admin) for
        // --for seconds, then restores it. Ctrl+C to stop (skips restore;
        // use `dns-restore` if DNS is left pointed at a dead filter).
        //   dns-serve [--port N] [--upstream IP]... [--enforce] [--for SECONDS]
        Some("dns-serve") => {
            let mut port: u16 = 5300;
            let mut upstreams = Vec::new();
            let mut enforce = false;
            let mut seconds: u64 = 60;
            let mut it = args.iter().skip(2);
            while let Some(flag) = it.next() {
                match flag.as_str() {
                    "--enforce" => enforce = true,
                    "--port" => port = it.next().and_then(|v| v.parse().ok()).unwrap_or(port),
                    "--for" => {
                        seconds = it.next().and_then(|v| v.parse().ok()).unwrap_or(seconds).min(600)
                    }
                    "--upstream" => {
                        if let Some(ip) = it.next().and_then(|v| v.parse::<std::net::IpAddr>().ok()) {
                            upstreams.push(std::net::SocketAddr::new(ip, 53));
                        }
                    }
                    _ => {}
                }
            }
            if enforce {
                port = dns_enforce::DNS_PORT;
            }
            let mut listen = vec![std::net::SocketAddr::from(([127, 0, 0, 1], port))];
            if enforce {
                listen.push(std::net::SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port)));
            }
            let shared = dns_filter::new_upstreams(upstreams);
            let cfg = dns_filter::Config {
                listen,
                upstreams: shared.clone(),
                on_block: std::sync::Arc::new(|name, entry| {
                    println!("BLOCKED {name} (matched {entry})");
                }),
            };
            let bound = match dns_filter::spawn(cfg) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("dns-serve failed: {e}");
                    std::process::exit(1);
                }
            };
            if enforce {
                println!("DNS filter up; taking over system DNS for {seconds}s.");
                match dns_enforce::run_manual(&bound, shared, seconds) {
                    Ok(()) => println!("Done; system DNS restored."),
                    Err(e) => {
                        eprintln!("dns-serve --enforce: {e}");
                        std::process::exit(1);
                    }
                }
            } else {
                println!("DNS filter listening on 127.0.0.1:{port} — Ctrl+C to stop.");
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(3600));
                }
            }
        }
        Some("dns-status") => {
            print!("{}", dns_enforce::status());
            print!("{}", browser_policy::status());
            print!("{}", firewall::status());
        }
        // Emergency hand-back of system DNS (e.g. the service crashed while
        // DNS pointed at the filter). Refused while a lock is active AND the
        // service is running: that combination means the filter is up and
        // this would just be a way around the lock.
        Some("dns-restore") => {
            let lock_active = db::open()
                .ok()
                .and_then(|c| db::lock::get_current(&c).ok())
                .map(|l| l.is_some())
                .unwrap_or(false);
            let service_running = std::process::Command::new("sc.exe")
                .args(["query", install::SERVICE_NAME])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).contains("RUNNING"))
                .unwrap_or(false);
            if lock_active && service_running {
                eprintln!("Refused: a lock is active and the Warden service is running.");
                std::process::exit(1);
            }
            match firewall::remove() {
                Ok(n) => println!("Removed {n} firewall rule(s)."),
                Err(e) => eprintln!("firewall rule removal failed: {e}"),
            }
            match browser_policy::restore() {
                Ok(n) => println!("Restored {n} browser policy value(s)."),
                Err(e) => eprintln!("browser policy restore failed: {e}"),
            }
            match dns_enforce::restore() {
                Ok(n) => println!("Restored DNS on {n} adapter(s)."),
                Err(e) => {
                    eprintln!("dns-restore failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        // Chrome launches native messaging hosts with the caller's origin
        // as argv[1], e.g. "chrome-extension://<id>/" — that's how this
        // case is told apart from an SCM launch (which passes no args).
        Some(arg) if arg.starts_with("chrome-extension://") => {
            native_messaging::run();
        }
        // No recognized args: this is how the Service Control Manager
        // actually launches us (binPath includes "run", but the dispatcher
        // doesn't care what's in argv — it just needs to be called).
        _ => {
            if let Err(e) = service::run() {
                logging::log_line(&format!("Fatal: StartServiceCtrlDispatcherW failed: {e:?}"));
                eprintln!(
                    "Failed to start service dispatcher: {e:?}\n\
                     (This binary is meant to be launched by the Windows Service Control \
                     Manager. Run with `install` to register it as a service.)"
                );
                std::process::exit(1);
            }
        }
    }
}
