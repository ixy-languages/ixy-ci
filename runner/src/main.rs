use std::io::{self, BufRead, Result};
use std::process::{self, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::{env, thread};

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    match args.next() {
        None => {
            println!("missing program to start");
            process::exit(-1);
        }
        Some(cmd) => {
            let mut child = Command::new(cmd)
                .stdout(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stdin(Stdio::piped())
                .args(args)
                .spawn()?;
            let cont = Arc::new(AtomicBool::new(true));

            let cont_clone = Arc::clone(&cont);
            thread::spawn(move || {
                io::stdin().lock().lines().next();
                cont_clone.store(false, Ordering::SeqCst);
            });

            while child.try_wait()?.is_none() {
                if !cont.load(Ordering::SeqCst) {
                    // Make sure to kill all descendants of child (e.g. of sudo)
                    unsafe {
                        let pgid = libc::getpgid(child.id() as i32);
                        libc::kill(-pgid, libc::SIGINT);
                    }
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
    Ok(())
}
