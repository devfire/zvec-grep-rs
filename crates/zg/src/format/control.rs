//! Daemon control status (`server on|off|status`).

/// Prints daemon control status (`server on|off|status`).
pub fn print_control_status(running: bool, ready: bool, pid: Option<u32>, url: Option<&str>) {
    println!("running: {}", if running { "yes" } else { "no" });
    println!("ready: {}", if ready { "yes" } else { "no" });
    if let Some(pid) = pid {
        println!("pid: {pid}");
    }
    if let Some(url) = url {
        println!("url: {url}");
    }
}
