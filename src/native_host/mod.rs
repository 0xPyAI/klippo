pub mod host;
pub use host::{
    is_registered,
    extension_dir, handle_request, is_native_host_invocation, register_native_host, register_native_host_for,
    run_native_host, unregister_native_host, BrowserTargets, BUNDLED_EXTENSION_ID, FIREFOX_EXTENSION_ID, HOST_NAME,
};
