// Same program as `wisprcheap`, built without a console window on Windows (desktop shortcut,
// background instance). When started from a terminal, its output goes to that terminal.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
    wisprcheap::cli::main(true);
}
