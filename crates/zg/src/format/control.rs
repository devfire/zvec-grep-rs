//! Daemon control status (`server on|off|status`).

/// Prints daemon control status (`server on|off|status`) straight from the
/// liveness snapshot: taking the struct (not a `(running, ready)` bool
/// pair) makes transposition unrepresentable at the call site.
pub fn print_control_status(status: &zg_server::server_controller::DaemonControlStatus) {
    println!("running: {}", if status.running { "yes" } else { "no" });
    println!("ready: {}", if status.ready { "yes" } else { "no" });
    if let Some(pid) = status.pid {
        println!("pid: {pid}");
    }
    if let Some(url) = status.server_url.as_deref() {
        println!("url: {url}");
    }
}
