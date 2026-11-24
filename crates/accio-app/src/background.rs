//! Resource policy for maintenance only. Interactive searches keep the normal Rayon pool.

use parking_lot::Mutex;
use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN};

pub fn enter() {
    // Background mode lowers CPU, I/O and memory scheduling priorities. It is not
    // inherited by newly spawned threads, so every maintenance worker enters it.
    if unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) } == 0 {
        log::warn!("background scheduling: {}", std::io::Error::last_os_error());
    }
}

pub struct Maintenance {
    pool: rayon::ThreadPool,
    gate: Mutex<()>,
}

impl Maintenance {
    pub fn new() -> Self {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(2));
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("maintenance-{i}"))
            .start_handler(|_| enter())
            .build()
            .expect("maintenance pool");
        Self { pool, gate: Mutex::new(()) }
    }

    pub fn run<T: Send>(&self, f: impl FnOnce() -> T + Send) -> T {
        // Serialize bulk disk/memory work across volumes. Journal updates do not
        // take this gate, and user searches never run on these low-priority threads.
        let _guard = self.gate.lock();
        self.pool.install(f)
    }
}
