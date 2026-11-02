// Default allocator: mimalloc. `--no-default-features --features rusty-alloc`
// selects the Rust allocator without building the C allocator (#5872).
// With neither feature the standard library system allocator is used.
#[cfg(all(feature = "mimalloc-allocator", not(feature = "rusty-alloc")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

// mimalloc tags its macOS VM regions with tag 100 by default, which macOS
// names `VM_MEMORY_IOACCELERATOR`. `footprint`, `vmmap` and Activity Monitor
// then report the whole heap as GPU memory ("61 MB IOAccelerator" at idle),
// which reads as AppKit/CoreAnimation initialising when nothing GPU-related
// runs. Retag to 254 (VM_MEMORY_APPLICATION_SPECIFIC_15) so the heap is
// labelled as the app's own memory. This must run before the first
// allocation, because the tag sticks to the arena mimalloc reserves then;
// setting it from `main` is already too late, so it runs as a Mach-O
// initializer. An explicit `MIMALLOC_OS_TAG` still wins.
#[cfg(all(
    target_os = "macos",
    feature = "mimalloc-allocator",
    not(feature = "rusty-alloc")
))]
#[used]
#[unsafe(link_section = "__DATA,__mod_init_func")]
static MIMALLOC_RETAG: extern "C" fn() = {
    extern "C" fn retag_mimalloc_heap() {
        unsafe extern "C" {
            fn mi_option_set(option: std::ffi::c_int, value: std::ffi::c_long);
        }
        /// `mi_option_os_tag` in mimalloc's `mi_option_e`.
        const MI_OPTION_OS_TAG: std::ffi::c_int = 18;
        if std::env::var_os("MIMALLOC_OS_TAG").is_none() {
            // SAFETY: mimalloc's option setter is callable before its own
            // initialisation; it only stores the value.
            unsafe { mi_option_set(MI_OPTION_OS_TAG, 254) };
        }
    }
    retag_mimalloc_heap
};

#[cfg(feature = "rusty-alloc")]
#[global_allocator]
static GLOBAL: rusty_alloc_api::RustyAlloc = rusty_alloc_api::RustyAlloc;

fn main() -> std::process::ExitCode {
    // Reset SIGPIPE to SIG_DFL so piping codewhale output into a command that
    // exits early (e.g. `codewhale doctor | head`) terminates the process
    // cleanly with exit code 141 instead of panicking on the broken-pipe
    // write. Many execution environments (systemd, Docker, some shells)
    // inherit SIGPIPE set to SIG_IGN, which makes write(2) return EPIPE;
    // Rust's `println!` then treats that io::Error as fatal and panics.
    // See issue #4030.
    // SAFETY: process entry; no threads or handlers yet.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    codewhale_runtime::resolve_state_from_environment();

    // Published codew/codewhale-tui compatibility names are copies or links
    // to this executable and use the same CLI parser and Engine entrypoint.
    codewhale_cli::run_cli()
}
