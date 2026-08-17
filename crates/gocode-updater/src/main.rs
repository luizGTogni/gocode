use std::{env, path::PathBuf, thread, time::Duration};

fn main() {
    let mut args = env::args_os().skip(1);
    let _pid = args
        .next()
        .and_then(|value| value.to_string_lossy().parse::<u32>().ok())
        .unwrap_or(0);
    let staged = args
        .next()
        .map_or_else(|| fail("missing staged executable"), PathBuf::from);
    let installed = args
        .next()
        .map_or_else(|| fail("missing installed executable"), PathBuf::from);

    // The launching gocode.exe is still exiting (its own exe file stays locked until it does),
    // so the rename below fails until that happens. Rather than guess a fixed delay, retry the
    // (side-effect-free-on-failure) replace on a bounded schedule until it succeeds.
    let mut last_error = String::new();
    let mut replaced = false;
    for _ in 0..100 {
        match gocode_updater::replace_with_rollback(&staged, &installed) {
            Ok(()) => {
                replaced = true;
                break;
            }
            Err(error) => {
                last_error = error.to_string();
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
    if !replaced {
        fail(&last_error);
    }
    if let Err(error) = gocode_updater::restart(&installed, &[]) {
        fail(&error.to_string());
    }
}
fn fail(message: &str) -> ! {
    eprintln!("Gocode update failed: {message}");
    std::process::exit(1)
}
