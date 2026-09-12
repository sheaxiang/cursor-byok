mod desktop;
#[cfg(not(dev))]
mod frontend;
mod resource_limits;
mod startup;
mod tray;

pub fn run() -> std::process::ExitCode {
    desktop::run()
}
