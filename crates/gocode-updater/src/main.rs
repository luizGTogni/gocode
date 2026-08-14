use std::{env, path::PathBuf, thread, time::Duration};

fn main() {
    let mut args = env::args_os().skip(1);
    let pid = args
        .next()
        .and_then(|value| value.to_string_lossy().parse::<u32>().ok())
        .unwrap_or(0);
    let staged = args
        .next()
        .map_or_else(|| fail("missing staged executable"), PathBuf::from);
    let installed = args
        .next()
        .map_or_else(|| fail("missing installed executable"), PathBuf::from);
    for _ in 0..100 {
        if pid == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    if let Err(error) = gocode_updater::replace_with_rollback(&staged, &installed) {
        fail(&error.to_string());
    }
    if let Err(error) = gocode_updater::restart(&installed, &[]) {
        fail(&error.to_string());
    }
}
fn fail(message: &str) -> ! {
    eprintln!("Gocode update failed: {message}");
    std::process::exit(1)
}
