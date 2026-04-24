use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn main() {
    println!("kaonic-plugin-sample starting");

    loop {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_secs())
            .unwrap_or_default();
        println!("kaonic-plugin-sample heartbeat ts={now}");
        thread::sleep(Duration::from_secs(5));
    }
}
